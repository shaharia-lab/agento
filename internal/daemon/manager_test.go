package daemon

import (
	"context"
	"encoding/xml"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strings"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
)

// recordedCall captures one command invocation seen by fakeRunner.
type recordedCall struct {
	Name string
	Args []string
}

// fakeRunner implements commandRunner with canned outputs and errors keyed by
// command name. Commands without a canned response succeed with empty output.
// failOnce errors fire on the first matching call only, for testing fallback
// paths (first call fails, the retry/fallback succeeds).
type fakeRunner struct {
	calls    []recordedCall
	outputs  map[string]string
	errors   map[string]error
	failOnce map[string]error
}

func newFakeRunner() *fakeRunner {
	return &fakeRunner{
		outputs:  map[string]string{},
		errors:   map[string]error{},
		failOnce: map[string]error{},
	}
}

// Run records the invocation and returns the canned response for cmd.Name.
func (f *fakeRunner) Run(_ context.Context, name string, args ...string) (string, error) {
	f.calls = append(f.calls, recordedCall{Name: name, Args: args})
	if err, ok := f.failOnce[name]; ok {
		delete(f.failOnce, name)
		return "", err
	}
	if err, ok := f.errors[name]; ok {
		return "", err
	}
	return f.outputs[name], nil
}

// argSeqs renders calls as "name arg arg" lines for compact comparisons.
func (f *fakeRunner) argSeqs() []string {
	seqs := make([]string, 0, len(f.calls))
	for _, c := range f.calls {
		seqs = append(seqs, c.Name+" "+strings.Join(c.Args, " "))
	}
	return seqs
}

// testOptions returns the fixed-path Options used by the golden render tests.
// It must not be handed to Install — the paths are not writable.
func testOptions() Options {
	return Options{
		BinaryPath: "/home/test/.local/bin/agento",
		DataDir:    "/home/test/.agento",
		LogPath:    "/home/test/.agento/logs/service.log",
		Port:       8990,
		ExtraPath:  "/home/test/.local/bin:/usr/local/bin:/usr/bin:/bin",
	}
}

// testOptionsIn returns writable Options rooted at home, for Install tests.
func testOptionsIn(home string) Options {
	return Options{
		BinaryPath: filepath.Join(home, "bin", "agento"),
		DataDir:    filepath.Join(home, ".agento"),
		LogPath:    filepath.Join(home, ".agento", "logs", "service.log"),
		Port:       8990,
		ExtraPath:  "/home/test/.local/bin:/usr/local/bin:/usr/bin:/bin",
	}
}

// stubPortFree replaces the port probe with a stub answering `used`.
func stubPortFree(t *testing.T, used bool) {
	t.Helper()
	prev := portInUse
	portInUse = func(int) bool { return used }
	t.Cleanup(func() { portInUse = prev })
}

// stubUID pins the uid seam to 501 so launchd domain-target assertions are
// deterministic regardless of the test process's real uid.
func stubUID(t *testing.T) {
	t.Helper()
	prev := currentUID
	currentUID = func() int { return 501 }
	t.Cleanup(func() { currentUID = prev })
}

func TestRenderPlistGolden(t *testing.T) {
	t.Parallel()
	got, err := render("agento.plist.tmpl", testOptions())
	if err != nil {
		t.Fatalf("render plist: %v", err)
	}
	golden, err := os.ReadFile(filepath.Join("testdata", "agento.plist.golden"))
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	if string(got) != string(golden) {
		t.Errorf("plist mismatch with golden file.\n--- got ---\n%s\n--- want ---\n%s", got, golden)
	}
}

// hostileOptions returns Options whose every path value carries a space, a
// double quote, a backslash, a percent, and XML metacharacters — the inputs
// that broke unescaped unit/plist rendering.
func hostileOptions() Options {
	return Options{
		BinaryPath: `/home/a "b"/bin\agento%i/agent&<>'".bin`,
		DataDir:    `/home/a "b"/data\dir%i/data&<>'"`,
		LogPath:    `/home/a "b"/data\dir%i/logs/service&<>'".log`,
		Port:       8990,
		ExtraPath:  `/opt/my tools/bin:/weird "q"\p%i/bin:/x&<>'"/bin`,
	}
}

func TestRenderSystemdUnitGolden(t *testing.T) {
	t.Parallel()
	got, err := render("agento.service.tmpl", testOptions())
	if err != nil {
		t.Fatalf("render unit: %v", err)
	}
	golden, err := os.ReadFile(filepath.Join("testdata", "agento.service.golden"))
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	if string(got) != string(golden) {
		t.Errorf("unit mismatch with golden file.\n--- got ---\n%s\n--- want ---\n%s", got, golden)
	}
}

func TestRenderSystemdUnitSpacesGolden(t *testing.T) {
	t.Parallel()
	opts := testOptions()
	opts.BinaryPath = "/home/test user/.local/bin/agento"
	opts.DataDir = "/home/test user/.agento"
	opts.LogPath = "/home/test user/.agento/logs/service.log"
	opts.ExtraPath = "/home/test user/.local/bin:/opt/My Tools/bin:/usr/bin"
	got, err := render("agento.service.tmpl", opts)
	if err != nil {
		t.Fatalf("render unit: %v", err)
	}
	golden, err := os.ReadFile(filepath.Join("testdata", "agento.service.spaces.golden"))
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	if string(got) != string(golden) {
		t.Errorf("unit mismatch with spaces golden.\n--- got ---\n%s\n--- want ---\n%s", got, golden)
	}
}

func TestRenderPlistEntitiesGolden(t *testing.T) {
	t.Parallel()
	opts := testOptions()
	opts.BinaryPath = "/home/a&b/.local/bin/agento"
	opts.DataDir = "/home/a&b/.agento"
	opts.LogPath = "/home/a&b/.agento/logs/service.log"
	opts.ExtraPath = "/home/a&b/.local/bin:/opt/A<B>/bin:/usr/bin"
	got, err := render("agento.plist.tmpl", opts)
	if err != nil {
		t.Fatalf("render plist: %v", err)
	}
	golden, err := os.ReadFile(filepath.Join("testdata", "agento.plist.entities.golden"))
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	if string(got) != string(golden) {
		t.Errorf("plist mismatch with entities golden.\n--- got ---\n%s\n--- want ---\n%s", got, golden)
	}
}

func TestRenderEscapesHostileValues(t *testing.T) {
	t.Parallel()

	t.Run("systemd unit", func(t *testing.T) {
		t.Parallel()
		got, err := render("agento.service.tmpl", hostileOptions())
		if err != nil {
			t.Fatalf("render unit: %v", err)
		}
		unit := string(got)
		// Every Environment= assignment and the ExecStart binary path must be
		// double-quoted as a whole, with \ " escaped and % doubled (systemd
		// would otherwise consume %i as a specifier).
		wants := []string{
			`ExecStart="/home/a \"b\"/bin\\agento%%i/agent&<>'\".bin" web --no-browser`,
			`Environment="PATH=/opt/my tools/bin:/weird \"q\"\\p%%i/bin:/x&<>'\"/bin"`,
			`Environment="AGENTO_DATA_DIR=/home/a \"b\"/data\\dir%%i/data&<>'\""`,
			`Environment="PORT=8990"`,
		}
		for _, want := range wants {
			if !strings.Contains(unit, want) {
				t.Errorf("unit missing %q:\n%s", want, unit)
			}
		}
		// No raw % may survive on an Environment=/ExecStart= line outside a
		// %% pair — a lone one would be expanded as a specifier at load time.
		// StandardOutput/StandardError take the remainder of the line
		// literally, so they are exempt (and out of scope per the issue).
		for _, line := range strings.Split(unit, "\n") {
			if !strings.HasPrefix(line, "Environment=") && !strings.HasPrefix(line, "ExecStart=") {
				continue
			}
			if strings.Contains(strings.ReplaceAll(line, "%%", ""), "%") {
				t.Errorf("line contains an unescaped %% specifier: %q", line)
			}
		}
	})

	t.Run("launchd plist", func(t *testing.T) {
		t.Parallel()
		got, err := render("agento.plist.tmpl", hostileOptions())
		if err != nil {
			t.Fatalf("render plist: %v", err)
		}
		plist := string(got)
		// The rendered plist must be well-formed XML — strip the DOCTYPE
		// because encoding/xml cannot resolve the external Apple DTD.
		idx := strings.Index(plist, "<plist")
		if idx < 0 {
			t.Fatalf("rendered plist lacks a <plist> element:\n%s", plist)
		}
		if err := xml.Unmarshal([]byte(plist[idx:]), new(any)); err != nil {
			t.Fatalf("rendered plist is not valid XML: %v\n%s", err, plist)
		}
		// Raw input characters that only survive when escaping failed: the
		// single quote of <>'" is never entity-escaped (xml.EscapeText emits
		// &#39;), so a literal ' means the value went in raw. Escaped
		// sequences (&amp;, &#34;) are present by design and not checked.
		for _, bad := range []string{"<B>", "'"} {
			if strings.Contains(plist, bad) {
				t.Errorf("plist contains unescaped sequence %q:\n%s", bad, plist)
			}
		}
	})
}

// TestRenderUnitVerifiedBySystemdAnalyze runs the real `systemd-analyze
// verify` against the space-containing render — the acceptance-criterion
// check that only a systemd host can perform. Skipped elsewhere (CI has no
// systemd); TestSystemdInstallSequenceAndIdempotency exercises the same
// verify hook through fakeRunner on every platform.
func TestSystemdInstallVerifyFailureAbortsInstall(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	// systemd-analyze exits 0 but reports a parse problem in its output — the
	// case a bare exit-code check would wave through.
	runner.outputs["systemd-analyze"] = "agento.service:6: Invalid syntax, ignoring: \"UNTERM=abc"
	mgr := newSystemdForHome(runner, home)

	err := mgr.Install(context.Background(), testOptionsIn(home))
	if err == nil || !strings.Contains(err.Error(), "reported problems") {
		t.Fatalf("want verify-reported-problems error, got %v", err)
	}
	// The unit must not be enabled when verification fails.
	for _, seq := range runner.argSeqs() {
		if strings.Contains(seq, "enable --now") {
			t.Errorf("service must not be enabled after failed verify; calls %v", runner.argSeqs())
		}
	}
}

func TestSystemdInstallVerifyCleanOutputProceeds(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	runner.outputs["systemd-analyze"] = "" // clean verify: no findings
	mgr := newSystemdForHome(runner, home)

	if err := mgr.Install(context.Background(), testOptionsIn(home)); err != nil {
		t.Fatalf("clean verify must not block install, got %v", err)
	}
}

func TestRenderUnitVerifiedBySystemdAnalyze(t *testing.T) {
	if _, err := exec.LookPath("systemd-analyze"); err != nil {
		t.Skip("systemd-analyze not available on this host")
	}
	// systemd-analyze verify insists the ExecStart binary exists and is
	// executable, so use a real (script) binary inside a space-containing
	// directory — also proving the quoted path is parsed as ONE command.
	// /bin/sh is guaranteed on any host that has systemd-analyze.
	tmp := t.TempDir()
	binDir := filepath.Join(tmp, "test user", "bin")
	if err := os.MkdirAll(binDir, 0o750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	binary := filepath.Join(binDir, "agento")
	if err := os.WriteFile(binary, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatalf("write stub binary: %v", err)
	}
	logDir := filepath.Join(tmp, "test user", ".agento", "logs")
	if err := os.MkdirAll(logDir, 0o750); err != nil {
		t.Fatalf("mkdir logs: %v", err)
	}
	opts := testOptions()
	opts.BinaryPath = binary
	opts.DataDir = filepath.Join(tmp, "test user", ".agento")
	opts.LogPath = filepath.Join(logDir, "service.log")
	opts.ExtraPath = binDir + ":/opt/My Tools/bin:/usr/bin"
	got, err := render("agento.service.tmpl", opts)
	if err != nil {
		t.Fatalf("render unit: %v", err)
	}
	unit := filepath.Join(tmp, "agento.service")
	if err := os.WriteFile(unit, got, 0o600); err != nil {
		t.Fatalf("write unit: %v", err)
	}
	cmd := exec.Command("systemd-analyze", "verify", unit) //nolint:gosec // fixed binary, test-rendered file
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("systemd-analyze verify rejected the spaced-value unit: %v\n%s\nunit:\n%s", err, out, got)
	}
}

func TestNewForTestPlatformSelection(t *testing.T) {
	t.Parallel()
	runner := newFakeRunner()
	home := t.TempDir()

	if _, err := NewForTest("darwin", runner, home); err != nil {
		t.Errorf("darwin: unexpected error: %v", err)
	}
	if _, err := NewForTest("linux", runner, home); err != nil {
		t.Errorf("linux: unexpected error: %v", err)
	}
	_, err := NewForTest("windows", runner, home)
	if !errors.Is(err, ErrUnsupportedOS) {
		t.Errorf("windows: want ErrUnsupportedOS, got %v", err)
	}
}

func TestCheckPortFreeRefusesOccupiedPort(t *testing.T) {
	stubPortFree(t, true)
	mgr := newSystemdForHome(newFakeRunner(), t.TempDir())

	err := mgr.Install(context.Background(), testOptions())
	if !errors.Is(err, ErrAlreadyRunning) {
		t.Fatalf("want ErrAlreadyRunning, got %v", err)
	}
	// Nothing may be written or executed when the port is busy.
	unit, _ := mgr.unitPath()
	if _, statErr := os.Stat(unit); !errors.Is(statErr, os.ErrNotExist) {
		t.Errorf("unit file must not be written on port conflict")
	}
}

func TestParseSystemdShowFixture(t *testing.T) {
	t.Parallel()
	fixture, err := os.ReadFile(filepath.Join("testdata", "systemctl_show_active.txt"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	st := parseSystemdShow(Status{}, string(fixture))
	if !st.Installed || !st.Enabled || !st.Running || st.PID != 4242 {
		t.Errorf("got %+v, want installed+enabled+running with PID 4242", st)
	}
}

func TestParseSystemdShowInactive(t *testing.T) {
	t.Parallel()
	out := "LoadState=loaded\nUnitFileState=disabled\nActiveState=inactive\nMainPID=0\n"
	st := parseSystemdShow(Status{}, out)
	if !st.Installed || st.Enabled || st.Running || st.PID != 0 {
		t.Errorf("got %+v, want installed only", st)
	}
}

func TestParseSystemdShowNotFound(t *testing.T) {
	t.Parallel()
	out := "LoadState=not-found\nUnitFileState=\nActiveState=inactive\nMainPID=0\n"
	st := parseSystemdShow(Status{}, out)
	if st.Installed || st.Enabled || st.Running {
		t.Errorf("got %+v, want all false for a missing unit", st)
	}
}

func TestIsEphemeralPath(t *testing.T) {
	t.Parallel()
	if !isEphemeralPath(filepath.Join(os.TempDir(), "go-build123", "agento")) {
		t.Error("temp-dir binary must be detected as ephemeral")
	}
	if isEphemeralPath("/home/user/.local/bin/agento") {
		t.Error("regular install path must not be flagged")
	}
}

func TestServiceLogPath(t *testing.T) {
	t.Parallel()
	cfg := &config.AppConfig{DataDir: "/data"}
	want := filepath.Join("/data", "logs", "service.log")
	if got := ServiceLogPath(cfg); got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestSystemdInstallSequenceAndIdempotency(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	mgr := newSystemdForHome(runner, home)
	ctx := context.Background()

	for i := 0; i < 2; i++ {
		if err := mgr.Install(ctx, testOptionsIn(home)); err != nil {
			t.Fatalf("install #%d: %v", i+1, err)
		}
	}
	unit, _ := mgr.unitPath()
	if _, err := os.Stat(unit); err != nil {
		t.Fatalf("unit file missing after install: %v", err)
	}
	// Content correctness is asserted by diffing render() output for these
	// exact options against the golden file — no need to re-read the file
	// (which trips gosec G304 on a variable path).
	rendered, err := render("agento.service.tmpl", testOptionsIn(home))
	if err != nil {
		t.Fatalf("re-render unit: %v", err)
	}
	golden, err := os.ReadFile(filepath.Join("testdata", "agento.service.golden"))
	if err != nil {
		t.Fatalf("read unit golden: %v", err)
	}
	want := strings.NewReplacer(
		"/home/test/.local/bin/agento", filepath.Join(home, "bin", "agento"),
		"/home/test/.agento", filepath.Join(home, ".agento"),
	).Replace(string(golden))
	if string(rendered) != want {
		t.Errorf("installed unit differs from golden:\n--- got ---\n%s\n--- want ---\n%s", rendered, want)
	}
	if !strings.Contains(string(rendered), "Restart=on-failure") {
		t.Errorf("unit lacks Restart=on-failure:\n%s", rendered)
	}

	seqs := runner.argSeqs()
	// The username the code resolves — $USER first, user.Current() fallback —
	// mirrored here so the assertion holds whether or not USER is set.
	wantUser := os.Getenv("USER")
	if wantUser == "" {
		if u, err := user.Current(); err == nil {
			wantUser = u.Username
		}
	}
	wantPerInstall := []string{
		// Install first probes Status to decide whether the port check applies.
		"systemctl --user show agento.service --property=LoadState,UnitFileState,ActiveState,MainPID",
		"systemd-analyze --version",
		"systemd-analyze verify " + unit,
		"systemctl --user daemon-reload",
		"systemctl --user enable --now agento.service",
		"loginctl enable-linger " + wantUser,
	}
	if len(seqs) != 2*len(wantPerInstall) {
		t.Fatalf("got calls %v, want 2x %v", seqs, wantPerInstall)
	}
	for i := 0; i < 2; i++ {
		for j, want := range wantPerInstall {
			if got := seqs[i*len(wantPerInstall)+j]; got != want {
				t.Errorf("install #%d call #%d: got %q, want %q", i+1, j+1, got, want)
			}
		}
	}

	// Uninstall twice: stops, removes the unit, leaves nothing behind.
	for i := 0; i < 2; i++ {
		if err := mgr.Uninstall(ctx); err != nil {
			t.Fatalf("uninstall #%d: %v", i+1, err)
		}
	}
	if _, err := os.Stat(unit); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("unit file still present after uninstall")
	}
}

func TestSystemdStartStopRestartSequences(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	mgr := newSystemdForHome(runner, home)
	ctx := context.Background()

	if err := mgr.Start(ctx); err != nil {
		t.Fatalf("start: %v", err)
	}
	if err := mgr.Stop(ctx); err != nil {
		t.Fatalf("stop: %v", err)
	}
	if err := mgr.Restart(ctx); err != nil {
		t.Fatalf("restart: %v", err)
	}
	want := []string{
		"systemctl --user start agento.service",
		"systemctl --user stop agento.service",
		"systemctl --user restart agento.service",
	}
	got := runner.argSeqs()
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("call #%d: got %q, want %q", i+1, got[i], want[i])
		}
	}
}

func TestSystemdStatusFromShowOutput(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	fixture, err := os.ReadFile(filepath.Join("testdata", "systemctl_show_active.txt"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	runner.outputs["systemctl"] = string(fixture)
	mgr := newSystemdForHome(runner, home)

	st, err := mgr.Status(context.Background())
	if err != nil {
		t.Fatalf("status: %v", err)
	}
	if !st.Installed || !st.Enabled || !st.Running || st.PID != 4242 {
		t.Errorf("got %+v, want installed+enabled+running PID 4242", st)
	}
}

func TestLaunchdInstallSequenceAndIdempotency(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	mgr := newLaunchdForHome(runner, home)
	ctx := context.Background()
	stubUID(t)

	for i := 0; i < 2; i++ {
		if err := mgr.Install(ctx, testOptionsIn(home)); err != nil {
			t.Fatalf("install #%d: %v", i+1, err)
		}
	}
	plist, _ := mgr.plistPath()
	if _, err := os.Stat(filepath.Join(home, "Library", "LaunchAgents", "com.shaharialab.agento.plist")); err != nil {
		t.Errorf("plist missing at expected path: %v", err)
	}
	seqs := runner.argSeqs()
	wantPerInstall := []string{
		// Install first probes Status to decide whether the port check applies.
		"launchctl print gui/501/com.shaharialab.agento",
		"launchctl bootout gui/501/com.shaharialab.agento",
		"launchctl bootstrap gui/501 " + plist,
	}
	if len(seqs) != 2*len(wantPerInstall) {
		t.Fatalf("got calls %v, want 2x %v", seqs, wantPerInstall)
	}
	for i := 0; i < 2; i++ {
		for j, want := range wantPerInstall {
			if got := seqs[i*len(wantPerInstall)+j]; got != want {
				t.Errorf("install #%d call #%d: got %q, want %q", i+1, j+1, got, want)
			}
		}
	}

	for i := 0; i < 2; i++ {
		if err := mgr.Uninstall(ctx); err != nil {
			t.Fatalf("uninstall #%d: %v", i+1, err)
		}
	}
	if _, err := os.Stat(plist); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("plist still present after uninstall")
	}
}

func TestLaunchdStartStopIdempotency(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	mgr := newLaunchdForHome(runner, home)
	ctx := context.Background()
	stubUID(t)

	// Start without install → clear "not installed" error.
	if err := mgr.Start(ctx); err == nil || !strings.Contains(err.Error(), "not installed") {
		t.Errorf("start without install: want not-installed error, got %v", err)
	}
	if err := mgr.Install(ctx, testOptionsIn(home)); err != nil {
		t.Fatalf("install: %v", err)
	}

	// Re-start an already-bootstrapped service is not an error.
	runner.errors["launchctl"] = fmt.Errorf("launchctl bootstrap gui/501/x: Bootstrap failed: 5: Input/output error")
	if err := mgr.Start(ctx); err != nil {
		t.Errorf("start on bootstrapped service should be a no-op, got %v", err)
	}
	// Stop on an already-unloaded service is not an error.
	runner.errors["launchctl"] = fmt.Errorf("launchctl bootout gui/501/x: Boot-out failed: 5: Input/output error")
	if err := mgr.Stop(ctx); err != nil {
		t.Errorf("stop on unloaded service should be a no-op, got %v", err)
	}
}

func TestLaunchdStatusParsesPrintOutput(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	stubUID(t)
	runner.outputs["launchctl"] = "gui/501/com.shaharialab.agento = {\n\tstate = running\n\tpid = 777\n}\n"
	mgr := newLaunchdForHome(runner, home)

	if err := mgr.Install(context.Background(), testOptionsIn(home)); err != nil {
		t.Fatalf("install: %v", err)
	}
	st, err := mgr.Status(context.Background())
	if err != nil {
		t.Fatalf("status: %v", err)
	}
	if !st.Installed || !st.Enabled || !st.Running || st.PID != 777 {
		t.Errorf("got %+v, want installed+enabled+running PID 777", st)
	}
}

func TestLaunchdStatusNotLoaded(t *testing.T) {
	home := t.TempDir()
	runner := newFakeRunner()
	stubUID(t)
	runner.errors["launchctl"] = fmt.Errorf("launchctl print: Could not find service")
	mgr := newLaunchdForHome(runner, home)

	st, err := mgr.Status(context.Background())
	if err != nil {
		t.Fatalf("status must not error when service is not loaded: %v", err)
	}
	if st.Installed || st.Enabled || st.Running || st.PID != 0 {
		t.Errorf("got %+v, want zero status", st)
	}
}

func TestSystemdInstallWhileManagedServiceRuns(t *testing.T) {
	// Regression: install must stay idempotent when the port's occupant is
	// our own already-running service — the port check may only refuse
	// foreign listeners. And because enable --now is a no-op on an active
	// unit, install must end with an explicit restart so the freshly written
	// unit (e.g. a moved binary path) actually takes effect — mirroring the
	// launchd bootout+bootstrap swap.
	stubPortFree(t, true) // port busy because the service itself is running
	home := t.TempDir()
	runner := newFakeRunner()
	fixture, err := os.ReadFile(filepath.Join("testdata", "systemctl_show_active.txt"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	runner.outputs["systemctl"] = string(fixture) // Status reports running
	mgr := newSystemdForHome(runner, home)

	if err := mgr.Install(context.Background(), testOptionsIn(home)); err != nil {
		t.Fatalf("install over own running service must succeed, got %v", err)
	}
	seqs := runner.argSeqs()
	last := seqs[len(seqs)-2] // final call is enable-linger
	if last != "systemctl --user restart agento.service" {
		t.Errorf("install over running service must restart to apply the new unit; got calls %v", seqs)
	}
}

func TestLaunchdRestartFallsBackToStart(t *testing.T) {
	stubPortFree(t, false)
	home := t.TempDir()
	runner := newFakeRunner()
	stubUID(t)
	mgr := newLaunchdForHome(runner, home)
	ctx := context.Background()

	if err := mgr.Install(ctx, testOptionsIn(home)); err != nil {
		t.Fatalf("install: %v", err)
	}
	// After Stop (bootout), kickstart cannot find the service — Restart must
	// fall back to Start (bootstrap) like systemd's restart-does-start.
	runner.calls = nil
	runner.failOnce["launchctl"] = fmt.Errorf("launchctl kickstart: Could not find service \"com.shaharialab.agento\"")
	if err := mgr.Restart(ctx); err != nil {
		t.Fatalf("restart after stop must fall back to start, got %v", err)
	}
	seqs := runner.argSeqs()
	if len(seqs) != 2 {
		t.Fatalf("got calls %v, want kickstart then bootstrap", seqs)
	}
	if !strings.HasPrefix(seqs[0], "launchctl kickstart -k ") {
		t.Errorf("first call: got %q, want kickstart", seqs[0])
	}
	if !strings.HasPrefix(seqs[1], "launchctl bootstrap gui/501 ") {
		t.Errorf("fallback call: got %q, want bootstrap", seqs[1])
	}
}
