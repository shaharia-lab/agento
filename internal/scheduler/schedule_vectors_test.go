package scheduler

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	// Vectors that turn on a DST transition are only reproducible where the
	// zone rules are. The embedded copy is a fallback for a machine with no
	// /usr/share/zoneinfo, not an override — the system database still wins
	// where it exists, so a genuine tzdata disagreement fails this test rather
	// than being papered over.
	_ "time/tzdata"

	"github.com/go-co-op/gocron/v2"
	"github.com/jonboulle/clockwork"

	"github.com/shaharia-lab/agento/internal/storage"
)

// The scheduler's next-fire-time computation, exported for the Rust port.
//
// The desktop app (desktop/, `desktop` branch) is porting this server to Rust,
// and issue #275 is the scheduler. The half of it that is verifiable before
// ownership of scheduling moves is *when a task fires*, and that is not a
// property of the cron string: `buildJobDefinition` picks one of four
// `gocron/v2` job types and **gocron's** semantics decide the answer. Three of
// those choices are silent when wrong:
//
//   - `run_immediately` is a one-time job at now+2s, not "now";
//   - `interval` with `every_days` and an `at_time` becomes a `DailyJob`, whose
//     next run is a wall-clock time in a location — not a 24h `DurationJob`;
//   - and when `at_time` fails to parse it falls back to that 24h duration job
//     **without an error**, so a typo changes the schedule and says nothing.
//
// Below the four types sit `robfig/cron`'s dialect (which gocron delegates to,
// with `CRON_TZ=` prepended), Go's `time.Date` normalization of wall-clock
// times a DST gap removed, and gocron's own guard against a fall-back hour
// firing a cron job twice. None of that is reproducible from a specification;
// it has to be pinned to what this Go build actually answers.
//
// So the vectors are generated here, from the real `gocron.Scheduler` driven by
// a fake clock, and asserted by both languages:
// desktop/src-tauri/src/native/schedule/ embeds the file with `include_str!`
// and asserts Rust produces the same. A change to gocron, to robfig/cron or to
// `buildJobDefinition` fails *this* test, in Go, rather than being discovered
// later from the Rust side.
//
// Regenerate with:
//
//	go test ./internal/scheduler/ -run TestScheduleVectors -update-scheduler-vectors
const scheduleVectorFile = "../../desktop/parity/scheduler_vectors.json"

var updateScheduleVectors = flag.Bool("update-scheduler-vectors", false,
	"rewrite "+scheduleVectorFile+" from this Go toolchain")

// nextRunCount is how many fire times each vector records. The first is the
// scheduler's own initial `nextRun` (`j.next(now)` advanced past now); the rest
// are successive `next` applications, which is what `Job.NextRuns` computes.
const nextRunCount = 4

// scheduleVector is one (schedule config, location, now) triple and everything
// observable about the job it produces.
type scheduleVector struct {
	Name string `json:"name"`
	Why  string `json:"why"`
	// IANA zone the gocron scheduler runs in. Production uses `time.Local`,
	// which is not reproducible across machines; the port takes the location as
	// a parameter and defaults it to the system zone, exactly as gocron does.
	Location       string                 `json:"location"`
	Now            string                 `json:"now"`
	ScheduleType   string                 `json:"schedule_type"`
	ScheduleConfig storage.ScheduleConfig `json:"schedule_config"`
	// Which gocron job type `buildJobDefinition` chose: one_time, duration,
	// daily or cron. Empty when the build itself failed. This is the field the
	// silent at_time fallback shows up in.
	Definition string `json:"definition"`
	// Stable classification of the failure, or "" on success. Not Go's error
	// text: the message is not part of any contract and Rust cannot reproduce
	// it, but *which* failure happened is exactly what has to agree.
	Error string `json:"error"`
	// Successive fire times, RFC 3339. A null is gocron's zero time, which is
	// how an exhausted schedule (a one-time job past its single run) answers.
	NextRuns []*string `json:"next_runs"`
	// Set only for `run_immediately`, whose fire time comes from `time.Now()`
	// inside `buildJobDefinition` and therefore cannot be frozen as an instant.
	// The offset from that instant is the whole content of the rule, so it is
	// recorded instead and `next_runs` is left empty.
	OneTimeOffsetMs *int64 `json:"one_time_offset_ms,omitempty"`
}

type scheduleVectorFileContents struct {
	Comment []string         `json:"_comment"`
	Cases   []scheduleVector `json:"cases"`
}

// scheduleVectorInput is one case before Go answers it.
type scheduleVectorInput struct {
	name     string
	why      string
	location string
	// Wall-clock "now", read in location.
	now [6]int
	typ storage.ScheduleType
	cfg storage.ScheduleConfig
}

// The anchors every case is written against. 2026-03-29 and 2026-10-25 are the
// EU transitions; 2026-03-08 and 2026-11-01 are the US ones.
var (
	utcNoon    = [6]int{2026, 2, 10, 9, 15, 30}
	berlinPre  = [6]int{2026, 3, 28, 12, 0, 0}  // day before the spring-forward
	berlinFall = [6]int{2026, 10, 25, 1, 30, 0} // inside the fall-back morning
	nycPre     = [6]int{2026, 3, 7, 12, 0, 0}   // day before the US spring-forward
	nycFall    = [6]int{2026, 11, 1, 0, 30, 0}  // inside the US fall-back morning
)

//nolint:funlen // a vector table: length is the point
func scheduleVectorInputs() []scheduleVectorInput {
	return []scheduleVectorInput{
		// --- run_immediately -------------------------------------------------
		{
			name: "run_immediately/plain", location: "UTC", now: utcNoon,
			typ: storage.ScheduleRunImmediately,
			why: "a one-time job at now+2s — not now, which gocron would refuse as already past",
		},
		{
			name: "run_immediately/config_is_ignored", location: "Europe/Berlin", now: berlinPre,
			typ: storage.ScheduleRunImmediately,
			cfg: storage.ScheduleConfig{EveryMinutes: 5, Expression: "0 0 * * *", RunAt: "2020-01-01T00:00:00Z"},
			why: "the branch is chosen on schedule_type alone; every config field is dead here",
		},

		// --- one_off ---------------------------------------------------------
		{
			name: "one_off/future", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			cfg: storage.ScheduleConfig{RunAt: "2026-06-01T12:00:00Z"},
			why: "the single run, then gocron's zero time — and note it OSCILLATES: next(zero) binary-searches back to the same run",
		},
		{
			name: "one_off/future_with_offset", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			cfg: storage.ScheduleConfig{RunAt: "2026-06-01T12:00:00+02:00"},
			why: "run_at carries its own zone and keeps it; the scheduler's location does not convert it",
		},
		{
			name: "one_off/past", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			cfg: storage.ScheduleConfig{RunAt: "2020-01-01T00:00:00Z"},
			why: "gocron drops past times at setup and then refuses the job outright",
		},
		{
			name: "one_off/exactly_now", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			cfg: storage.ScheduleConfig{RunAt: "2026-02-10T09:15:30Z"},
			why: "the past filter is a binary search that skips an exact hit, so now is already too late",
		},
		{
			name: "one_off/malformed", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			cfg: storage.ScheduleConfig{RunAt: "tomorrow"},
			why: "run_at is parsed as RFC3339 and nothing else",
		},
		{
			name: "one_off/empty", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			why: "an absent run_at fails the parse rather than defaulting",
		},
		{
			name: "one_off/date_only", location: "UTC", now: utcNoon,
			typ: storage.ScheduleOneOff,
			cfg: storage.ScheduleConfig{RunAt: "2026-06-01"},
			why: "RFC3339 needs the time; a bare date is not accepted",
		},

		// --- interval: duration ----------------------------------------------
		{
			name: "interval/every_minutes", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryMinutes: 5},
			why: "a plain duration job: absolute addition, no wall clock involved",
		},
		{
			name: "interval/every_minutes_wins_over_hours_and_days", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryMinutes: 5, EveryHours: 2, EveryDays: 3, AtTime: "09:00"},
			why: "the builder tests minutes, then hours, then days — the first positive one wins outright",
		},
		{
			name: "interval/every_hours", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryHours: 6},
			why: "hours reached only because minutes was zero",
		},
		{
			name: "interval/every_days_no_at_time", location: "Europe/Berlin", now: berlinPre,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1},
			why: "without at_time this is 24 absolute hours, so it walks off the wall clock across a DST transition",
		},
		{
			name: "interval/every_days_two_no_at_time", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 2},
			why: "days multiply into 24h each",
		},
		{
			name: "interval/negative_minutes_falls_through", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryMinutes: -5},
			why: "the guards are `> 0`, so a negative value is not rejected as such — it is simply never chosen, and the config then has no interval at all",
		},
		{
			name: "interval/all_zero", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			why: "the one interval config that fails to build at all",
		},
		{
			name: "interval/at_time_without_days", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{AtTime: "09:00"},
			why: "at_time is only read inside the every_days branch; alone it schedules nothing",
		},

		// --- interval: the DailyJob, and the silent fallback ------------------
		{
			name: "interval/daily_at_time_later_today", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "18:30"},
			why: "first pass takes the same day when the at-time is still ahead",
		},
		{
			name: "interval/daily_at_time_already_passed", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "07:00"},
			why: "first pass finds nothing, so the second starts at midnight of day+interval",
		},
		{
			name: "interval/daily_every_three_days", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 3, AtTime: "07:00"},
			why: "interval > 1 jumps whole intervals; it does not run tomorrow and then wait",
		},
		{
			name: "interval/daily_midnight", location: "Europe/Berlin", now: berlinPre,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "00:00"},
			why: "a daily job holds the wall clock across the spring-forward, unlike the 24h duration job above",
		},
		{
			name: "interval/daily_inside_the_spring_forward_gap", location: "Europe/Berlin", now: berlinPre,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "02:30"},
			why: "02:30 does not exist on 2026-03-29; the default DST policy takes whatever time.Date normalizes to",
		},
		{
			name: "interval/daily_inside_the_fall_back_hour", location: "Europe/Berlin", now: berlinFall,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "02:30"},
			why: "02:30 happens twice on 2026-10-25; time.Date picks one of them and the job does not repeat",
		},
		{
			name: "interval/daily_inside_the_us_spring_forward_gap", location: "America/New_York", now: nycPre,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "02:30"},
			why: "the same gap a week earlier and five hours west — the offset arithmetic differs, the rule does not",
		},
		{
			name: "interval/daily_inside_the_us_fall_back_hour", location: "America/New_York", now: nycFall,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "01:30"},
			why: "01:30 happens twice on 2026-11-01; the daily job takes one of them and moves on",
		},
		{
			name: "interval/daily_at_time_23_59", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "23:59"},
			why: "the inclusive upper bound of the at_time range check",
		},
		{
			name: "interval/at_time_hour_out_of_range", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 3, AtTime: "25:00"},
			why: "THE SILENT FALLBACK: the daily job is rejected and a 72h duration job is scheduled with no error",
		},
		{
			name: "interval/at_time_minute_out_of_range", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "09:60"},
			why: "same fallback, minute bound",
		},
		{
			name: "interval/at_time_no_colon", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "9"},
			why: "same fallback, split length",
		},
		{
			name: "interval/at_time_three_parts", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "09:00:00"},
			why: "same fallback: HH:MM:SS looks right and is not accepted",
		},
		{
			name: "interval/at_time_non_numeric", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "ab:cd"},
			why: "same fallback, Atoi",
		},
		{
			name: "interval/at_time_negative_hour", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "-1:00"},
			why: "Atoi accepts the sign, so this is caught by the range check rather than the parse",
		},
		{
			name: "interval/at_time_padded_and_unpadded_agree", location: "UTC", now: utcNoon,
			typ: storage.ScheduleInterval,
			cfg: storage.ScheduleConfig{EveryDays: 1, AtTime: "7:5"},
			why: "Atoi does not require zero padding, so 7:5 is 07:05 and not a fallback",
		},

		// --- cron ------------------------------------------------------------
		{
			name: "cron/every_five_minutes", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "*/5 * * * *"},
			why: "the baseline five-field spec; note the first run truncates the seconds now carries",
		},
		{
			name: "cron/weekdays_at_nine", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 9 * * 1-5"},
			why: "a day-of-week range, and the weekend skip that comes with it",
		},
		{
			name: "cron/named_weekday", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 0 * * SUN"},
			why: "robfig accepts three-letter day names, case-insensitively",
		},
		{
			name: "cron/named_month", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 0 1 jan *"},
			why: "and month names",
		},
		{
			name: "cron/question_mark", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 0 ? * *"},
			why: "'?' is a synonym for '*' and, like '*', sets the star bit that makes dom/dow an AND",
		},
		{
			name: "cron/dom_and_dow_both_restricted", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 0 13 * 5"},
			why: "with neither field starred the two are ORed, so this is every 13th AND every Friday",
		},
		{
			name: "cron/step_from_a_value", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 9/4 * * *"},
			why: "'N/step' means 'N-max/step', which is robfig's own extension and not POSIX",
		},
		{
			name: "cron/list_of_values", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0,30 8,20 * * *"},
			why: "comma lists in two fields at once",
		},
		{
			name: "cron/monthly_first", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 0 1 * *"},
			why: "a month step, which walks the calendar rather than adding 30 days",
		},
		{
			name: "cron/descriptor_daily", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@daily"},
			why: "descriptors ARE accepted: gocron uses ParseStandard, which enables them",
		},
		{
			name: "cron/descriptor_midnight", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@midnight"},
			why: "an alias of @daily",
		},
		{
			name: "cron/descriptor_hourly", location: "Europe/Berlin", now: berlinFall,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@hourly"},
			why: "hourly across the fall-back, where the same wall-clock hour occurs twice",
		},
		{
			name: "cron/descriptor_weekly", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@weekly"},
			why: "Sunday midnight",
		},
		{
			name: "cron/descriptor_monthly", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@monthly"},
			why: "first of the month",
		},
		{
			name: "cron/descriptor_yearly", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@annually"},
			why: "@annually and @yearly are the same spec",
		},
		{
			name: "cron/descriptor_every_duration", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@every 1h30m"},
			why: "'@every' is not a spec at all: it is Go's ParseDuration behind a constant-delay schedule",
		},
		{
			name: "cron/descriptor_every_sub_second", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@every 100ms"},
			why: "Every() floors the delay at one second and drops the sub-second part of it",
		},
		{
			name: "cron/descriptor_every_bad_duration", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@every soon"},
			why: "a bad duration is a parse failure, not an unrecognized descriptor",
		},
		{
			name: "cron/descriptor_unknown", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "@fortnightly"},
			why: "anything else beginning with '@' is rejected",
		},
		{
			name: "cron/six_fields_rejected", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 * * * * *"},
			why: "Agento passes withSeconds=false, so a seconds field is one field too many",
		},
		{
			name: "cron/four_fields_rejected", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 9 * *"},
			why: "and exactly five are required, not at least five",
		},
		{
			name: "cron/garbage", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "not a cron"},
			why: "a plain parse failure",
		},
		{
			name: "cron/empty", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			why: "an empty expression reaches the parser as an empty field list, not as a special case",
		},
		{
			name: "cron/out_of_range_field", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 25 * * *"},
			why: "bounds are checked per field at parse time",
		},
		{
			name: "cron/never_fires", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 0 30 2 *"},
			why: "February 30th parses fine and matches nothing; gocron rejects it on the five-year search, not the parse",
		},
		{
			name: "cron/embedded_timezone_wins", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "CRON_TZ=America/New_York 0 9 * * *"},
			why: "a CRON_TZ prefix beats the scheduler's location, which is otherwise prepended as one",
		},
		{
			name: "cron/embedded_timezone_tz_form", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "TZ=Asia/Tokyo 30 3 * * *"},
			why: "the older TZ= spelling is accepted too",
		},
		{
			name: "cron/embedded_bad_timezone", location: "UTC", now: utcNoon,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "CRON_TZ=Mars/Olympus 0 9 * * *"},
			why: "an unloadable zone is a parse failure",
		},
		{
			name: "cron/across_the_spring_forward_gap", location: "Europe/Berlin", now: berlinPre,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 2 * * *"},
			why: "02:00 does not exist on 2026-03-29, and the hour loop steps absolute hours — so the job SKIPS that day",
		},
		{
			name: "cron/across_the_fall_back_hour", location: "Europe/Berlin", now: berlinFall,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "0 2 * * *"},
			why: "02:00 happens twice on 2026-10-25; gocron's duplicate-wall-clock guard suppresses the second",
		},
		{
			name: "cron/across_the_us_spring_forward_gap", location: "America/New_York", now: nycPre,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "30 2 * * *"},
			why: "the same skip on the US transition",
		},
		{
			name: "cron/across_the_us_fall_back_hour", location: "America/New_York", now: nycFall,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "30 1 * * *"},
			why: "the duplicate-wall-clock guard again, on the US transition",
		},
		{
			name: "cron/minute_zero_across_fall_back", location: "Europe/Berlin", now: berlinFall,
			typ: storage.ScheduleCron,
			cfg: storage.ScheduleConfig{Expression: "*/30 * * * *"},
			why: "a sub-hourly spec does repeat through the fall-back hour; the guard only catches an identical wall clock",
		},

		// --- unknown ---------------------------------------------------------
		{
			name: "unknown/weekly", location: "UTC", now: utcNoon,
			typ: storage.ScheduleType("weekly"),
			why: "the default arm: an unrecognized schedule_type is refused rather than defaulted",
		},
		{
			name: "unknown/empty", location: "UTC", now: utcNoon,
			typ: storage.ScheduleType(""),
			why: "and so is an absent one",
		},
	}
}

// definitionKinds maps gocron's unexported definition types onto the names the
// vectors use. Reading the concrete type is the only way to observe which
// branch `buildJobDefinition` took, and that branch *is* the silent-fallback
// trap. An unknown type fails loudly rather than being recorded as a new kind.
var definitionKinds = map[string]string{
	"gocron.oneTimeJobDefinition":  "one_time",
	"gocron.durationJobDefinition": "duration",
	"gocron.dailyJobDefinition":    "daily",
	"gocron.cronJobDefinition":     "cron",
}

// scheduleErrors classifies the failures `gocron.Scheduler.NewJob` can return
// for the four definitions Agento builds. The message text is deliberately not
// the contract — Rust cannot reproduce it — but which failure occurred is.
var scheduleErrors = []struct {
	err  error
	name string
}{
	{gocron.ErrOneTimeJobStartDateTimePast, "schedule:one_time_past"},
	{gocron.ErrCronJobParse, "schedule:cron_parse"},
	{gocron.ErrCronJobInvalid, "schedule:cron_invalid"},
	{gocron.ErrDurationJobIntervalZero, "schedule:duration_zero"},
	{gocron.ErrDurationJobIntervalNegative, "schedule:duration_negative"},
	{gocron.ErrDailyJobZeroInterval, "schedule:daily_zero_interval"},
	{gocron.ErrDailyJobHours, "schedule:daily_hours"},
	{gocron.ErrDailyJobMinutesSeconds, "schedule:daily_minutes_seconds"},
}

// buildErrorClass names the failure `buildJobDefinition` itself returned.
//
// It is derived from the schedule type rather than from the message, because
// each type has exactly one build-time failure: one_off can only fail parsing
// run_at, interval can only fail with no positive interval, and anything else
// is the default arm. Keep this in step if a second failure is ever added to
// one of those branches.
func buildErrorClass(typ storage.ScheduleType) string {
	switch typ {
	case storage.ScheduleOneOff:
		return "build:run_at"
	case storage.ScheduleInterval:
		return "build:invalid_interval"
	case storage.ScheduleRunImmediately, storage.ScheduleCron:
		return "build:unexpected"
	default:
		return "build:unknown_type"
	}
}

// runImmediatelyOffsetMs recovers how far ahead of the build instant a
// `run_immediately` job is scheduled, which is the only thing about it that is
// reproducible: `buildJobDefinition` reads `time.Now()` itself, so the absolute
// instant differs on every run and cannot be frozen.
//
// The offset is rounded to the second because `builtAt` was sampled just before
// the call rather than inside it; the assertion below is what makes the
// rounding safe rather than a way of hiding a wrong answer.
func runImmediatelyOffsetMs(t *testing.T, name string, def gocron.JobDefinition, builtAt time.Time) *int64 {
	t.Helper()

	cron, err := gocron.NewScheduler(gocron.WithClock(clockwork.NewFakeClockAt(builtAt)))
	if err != nil {
		t.Fatalf("%s: creating scheduler: %v", name, err)
	}
	defer func() {
		if shutErr := cron.Shutdown(); shutErr != nil {
			t.Errorf("%s: shutting down: %v", name, shutErr)
		}
	}()
	cron.Start()

	job, err := cron.NewJob(def, gocron.NewTask(func() {}))
	if err != nil {
		t.Fatalf("%s: scheduling: %v", name, err)
	}
	next, err := job.NextRun()
	if err != nil {
		t.Fatalf("%s: NextRun: %v", name, err)
	}

	raw := next.Sub(builtAt)
	if raw < 1500*time.Millisecond || raw > 2500*time.Millisecond {
		t.Fatalf("%s: run_immediately is %v out, want ~2s", name, raw)
	}
	offset := raw.Round(time.Second).Milliseconds()
	return &offset
}

// answer runs one input through the real gocron scheduler on a fake clock and
// records everything observable about the job it produced.
func answer(t *testing.T, in scheduleVectorInput) scheduleVector {
	t.Helper()

	loc, err := time.LoadLocation(in.location)
	if err != nil {
		t.Fatalf("%s: loading location %q: %v", in.name, in.location, err)
	}
	now := time.Date(in.now[0], time.Month(in.now[1]), in.now[2], in.now[3], in.now[4], in.now[5], 0, loc)

	out := scheduleVector{
		Name: in.name, Why: in.why, Location: in.location,
		Now:          now.Format(time.RFC3339Nano),
		ScheduleType: string(in.typ), ScheduleConfig: in.cfg,
		NextRuns: []*string{},
	}

	task := &storage.ScheduledTask{ID: in.name, ScheduleType: in.typ, ScheduleConfig: in.cfg}
	builtAt := time.Now()
	def, buildErr := (&Scheduler{}).buildJobDefinition(task)
	if buildErr != nil {
		out.Error = buildErrorClass(in.typ)
		return out
	}

	kind, ok := definitionKinds[fmt.Sprintf("%T", def)]
	if !ok {
		t.Fatalf("%s: gocron returned an unmapped definition type %T — add it to definitionKinds", in.name, def)
	}
	out.Definition = kind

	if in.typ == storage.ScheduleRunImmediately {
		out.OneTimeOffsetMs = runImmediatelyOffsetMs(t, in.name, def, builtAt)
		return out
	}

	cron, err := gocron.NewScheduler(
		gocron.WithClock(clockwork.NewFakeClockAt(now)),
		gocron.WithLocation(loc),
	)
	if err != nil {
		t.Fatalf("%s: creating scheduler: %v", in.name, err)
	}
	cron.Start()
	defer func() {
		if shutErr := cron.Shutdown(); shutErr != nil {
			t.Errorf("%s: shutting down: %v", in.name, shutErr)
		}
	}()

	job, err := cron.NewJob(def, gocron.NewTask(func() {}))
	if err != nil {
		for _, class := range scheduleErrors {
			if errors.Is(err, class.err) {
				out.Error = class.name
				return out
			}
		}
		t.Fatalf("%s: unclassified scheduling error %v — add it to scheduleErrors", in.name, err)
	}

	runs, err := job.NextRuns(nextRunCount)
	if err != nil {
		t.Fatalf("%s: NextRuns: %v", in.name, err)
	}
	for _, r := range runs {
		if r.IsZero() {
			out.NextRuns = append(out.NextRuns, nil)
			continue
		}
		formatted := r.Format(time.RFC3339Nano)
		out.NextRuns = append(out.NextRuns, &formatted)
	}
	return out
}

func TestScheduleVectors(t *testing.T) {
	inputs := scheduleVectorInputs()
	want := scheduleVectorFileContents{
		Comment: []string{
			"When an Agento scheduled task fires, as gocron/v2 actually computes it.",
			"",
			"Generated from Go (go test ./internal/scheduler/ -update-scheduler-vectors)",
			"and asserted by both languages: internal/scheduler/schedule_vectors_test.go",
			"regenerates and checks it against a real gocron.Scheduler on a fake clock,",
			"and desktop/src-tauri/src/native/schedule/ embeds it with include_str! and",
			"asserts the Rust port answers the same.",
			"",
			"'definition' is which of gocron's four job types buildJobDefinition chose —",
			"the field the silent at_time fallback shows up in. 'error' is a stable",
			"classification, not Go's message: 'build:*' is Agento's own builder",
			"refusing, 'schedule:*' is gocron refusing the definition it was handed.",
			"'next_runs[0]' is the scheduler's initial next run; the rest are successive",
			"applications of the schedule's next(). A null is gocron's zero time, which",
			"is how an exhausted one-time job answers — and the sequence then oscillates,",
			"because next(zero) binary-searches straight back to the single run.",
			"'one_time_offset_ms' replaces next_runs for run_immediately alone, whose",
			"fire time is time.Now()+2s and so has no absolute instant to freeze.",
			"",
			"Location is explicit because production uses time.Local, which is not",
			"reproducible across machines. Cases anchored on 2026-03-08, 2026-03-29,",
			"2026-10-25 and 2026-11-01 sit on DST transitions on purpose.",
		},
		Cases: make([]scheduleVector, 0, len(inputs)),
	}

	seen := make(map[string]bool, len(inputs))
	for _, in := range inputs {
		if seen[in.name] {
			t.Fatalf("duplicate vector name %q", in.name)
		}
		seen[in.name] = true
		want.Cases = append(want.Cases, answer(t, in))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding schedule vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateScheduleVectors {
		if mkErr := os.MkdirAll(filepath.Dir(scheduleVectorFile), 0o750); mkErr != nil {
			t.Fatalf("creating vector directory: %v", mkErr)
		}
		if writeErr := os.WriteFile(scheduleVectorFile, encoded, 0o600); writeErr != nil {
			t.Fatalf("writing %s: %v", scheduleVectorFile, writeErr)
		}
		t.Logf("wrote %s (%d cases)", scheduleVectorFile, len(want.Cases))
		return
	}

	onDisk, err := os.ReadFile(scheduleVectorFile) //nolint:gosec // fixed test path
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-scheduler-vectors): %v", scheduleVectorFile, err)
	}
	if string(onDisk) != string(encoded) {
		t.Errorf("%s is stale — regenerate with:\n"+
			"\tgo test ./internal/scheduler/ -run TestScheduleVectors -update-scheduler-vectors\n"+
			"Something moved in buildJobDefinition, in gocron/v2, in robfig/cron or in this "+
			"machine's tzdata. The Rust port in desktop/src-tauri/src/native/schedule/ "+
			"embeds this file and will fail against it, so check what changed before "+
			"accepting the new bytes.", scheduleVectorFile)
	}
}

// The three traps named in issue #275, asserted here as well as in the vector
// table — so a regression fails with the name of the behavior that broke
// rather than as one line of a diff between two large JSON files.
func TestTheThreeSilentSchedulingBehaviors(t *testing.T) {
	s := &Scheduler{}

	t.Run("run_immediately is a one-time job two seconds out", func(t *testing.T) {
		before := time.Now()
		def, err := s.buildJobDefinition(&storage.ScheduledTask{ScheduleType: storage.ScheduleRunImmediately})
		if err != nil {
			t.Fatalf("building: %v", err)
		}
		if got := fmt.Sprintf("%T", def); got != "gocron.oneTimeJobDefinition" {
			t.Fatalf("definition is %s, want a one-time job", got)
		}
		// The two seconds are not cosmetic: a one-time job whose single time is
		// not strictly in the future is refused at setup, so "now" would never
		// run at all.
		cron, err := gocron.NewScheduler(gocron.WithClock(clockwork.NewFakeClockAt(before)))
		if err != nil {
			t.Fatalf("creating scheduler: %v", err)
		}
		defer func() { _ = cron.Shutdown() }()
		cron.Start()
		job, err := cron.NewJob(def, gocron.NewTask(func() {}))
		if err != nil {
			t.Fatalf("scheduling: %v", err)
		}
		next, err := job.NextRun()
		if err != nil {
			t.Fatalf("NextRun: %v", err)
		}
		if delta := next.Sub(before); delta < 1500*time.Millisecond || delta > 2500*time.Millisecond {
			t.Fatalf("first run is %v out, want ~2s", delta)
		}
	})

	t.Run("a malformed at_time falls back to a duration job with no error", func(t *testing.T) {
		def, err := s.buildJobDefinition(&storage.ScheduledTask{
			ScheduleType:   storage.ScheduleInterval,
			ScheduleConfig: storage.ScheduleConfig{EveryDays: 1, AtTime: "9am"},
		})
		if err != nil {
			t.Fatalf("the fallback is silent by design, but it errored: %v", err)
		}
		if got := fmt.Sprintf("%T", def); got != "gocron.durationJobDefinition" {
			t.Fatalf("definition is %s, want the 24h duration fallback", got)
		}
	})

	t.Run("every_days with a valid at_time is a daily job, not 24 hours", func(t *testing.T) {
		def, err := s.buildJobDefinition(&storage.ScheduledTask{
			ScheduleType:   storage.ScheduleInterval,
			ScheduleConfig: storage.ScheduleConfig{EveryDays: 1, AtTime: "09:00"},
		})
		if err != nil {
			t.Fatalf("building: %v", err)
		}
		if got := fmt.Sprintf("%T", def); got != "gocron.dailyJobDefinition" {
			t.Fatalf("definition is %s, want a daily job — a duration job would drift off the wall clock", got)
		}
	})
}
