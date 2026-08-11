package claudesessions

import (
	"context"
	"database/sql"
	"errors"
	"log/slog"
	"runtime"
	"sync"
)

// Applying a scan's changes: read in parallel, write in batches.
//
// The scan used to be strictly serial and to open one transaction per file.
// Reading is I/O plus JSON decoding — 17.2 seconds for 1,671 transcripts on the
// reference machine, single-threaded, and that is a floor measured by a harness
// doing less work than the real reader. Writing is the opposite: modernc's
// SQLite serializes writers, so parallelism buys nothing there, but a
// transaction per file does not — a full re-read of 5,000 sessions is 5,000
// commits, each with its own fsync.
//
// So: a bounded pool of readers, and one writer draining them in batches. The
// two triggers for a full re-read are routine rather than exotic — any pricing
// rate edit and any idle-threshold change invalidate every cached row — so this
// is the difference between a settings save costing seconds and costing
// minutes.

// scanBatchSize is how many files one write transaction covers.
//
// Large enough that the per-commit cost is amortized to nothing, small enough
// that a failure loses a bounded amount of work and that a reader is never
// blocked long behind the writer.
const scanBatchSize = 100

// scanReaders bounds the reader pool.
//
// One less than GOMAXPROCS, floored at 2 and capped at 8: this runs on the
// user's own laptop while they are working, and a pool wide enough to saturate
// every core reading a multi-gigabyte corpus is felt as the machine getting
// slower. The cap is where the returns flatten anyway — past it the work is
// bound by the single writer and by the page cache, not by decode throughput.
func scanReaders() int {
	n := runtime.GOMAXPROCS(0) - 1
	switch {
	case n < 2:
		return 2
	case n > 8:
		return 8
	default:
		return n
	}
}

// scanUnit is one file to re-read, with what the diff already knows about it.
type scanUnit struct {
	df    diskFile
	isNew bool
}

// scanResult is a decoded transcript on its way to the writer.
//
// summary is nil when the file could not be read, which is not fatal: a
// transcript being written to right now, or one with a permission problem, must
// not abort the scan for every other session.
type scanResult struct {
	unit    scanUnit
	summary *ClaudeSessionSummary
	meta    subagentMeta // sub-agents only
}

// execer is the subset of *sql.DB and *sql.Tx the row writers need, so one
// implementation serves both the batched path and any direct call.
type execer interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
}

func applyChangesWithNotify(
	db *sql.DB, logger *slog.Logger,
	onDisk map[string]diskFile,
	diff diskDiff,
	notify func(sessionID, filePath string, isNew bool),
	progress func(done, total int),
) {
	total := len(diff.toInsert) + len(diff.toUpdate)
	if total > 0 || len(diff.toDelete) > 0 {
		logger.Info("claude sessions: incremental scan",
			"new", len(diff.toInsert),
			"modified", len(diff.toUpdate),
			"deleted", len(diff.toDelete),
			"unchanged", len(onDisk)-total,
			"readers", scanReaders())
	}

	units := make([]scanUnit, 0, total)
	for _, fp := range diff.toInsert {
		units = append(units, scanUnit{df: onDisk[fp], isNew: true})
	}
	for _, fp := range diff.toUpdate {
		units = append(units, scanUnit{df: onDisk[fp], isNew: false})
	}

	pending := readAndWrite(db, logger, units, progress)

	// Insights are computed per session over the parent transcript plus all of
	// its sub-agent transcripts, so a session is worth notifying about at most
	// once per scan however many of its files changed. Collected during the
	// write and emitted after: a session with N changed sub-agents would
	// otherwise enqueue N+1 items that each re-read all N+1 files, and on a
	// first scan the resulting fan-out overflows the worker queue.
	if notify != nil {
		for sessionID, p := range pending {
			notify(sessionID, p.filePath, p.isNew)
		}
	}

	deleteCachedFiles(db, logger, diff.toDelete)
}

// readAndWrite runs the reader pool and the single batching writer, returning
// the notifications the writer collected.
func readAndWrite(
	db *sql.DB, logger *slog.Logger, units []scanUnit, progress func(done, total int),
) map[string]pendingNotify {
	pending := map[string]pendingNotify{}
	if len(units) == 0 {
		if progress != nil {
			progress(0, 0)
		}
		return pending
	}

	work := make(chan scanUnit)
	// Buffered by one batch so readers keep going while the writer commits.
	results := make(chan scanResult, scanBatchSize)

	var readers sync.WaitGroup
	for range scanReaders() {
		readers.Add(1)
		go func() {
			defer readers.Done()
			for u := range work {
				results <- readUnit(u, logger)
			}
		}()
	}

	go func() {
		defer close(work)
		for _, u := range units {
			work <- u
		}
	}()
	go func() {
		readers.Wait()
		close(results)
	}()

	// The writer runs on this goroutine: SQLite serializes writers anyway, and
	// keeping it here means the scan is finished when this function returns.
	writeBatches(db, logger, results, len(units), pending, progress)
	return pending
}

// readUnit decodes one transcript. It touches no database, which is what makes
// it safe to run in parallel.
func readUnit(u scanUnit, logger *slog.Logger) scanResult {
	res := scanResult{unit: u}
	var err error
	if u.df.isSubagent {
		res.summary, _, err = readSubagentSummary(u.df.sessionID, u.df.projectPath, u.df.filePath, logger)
		if err == nil && res.summary != nil {
			res.meta = readSubagentMeta(u.df.filePath, logger)
		}
	} else {
		res.summary, _, err = readSessionSummary(u.df.sessionID, u.df.projectPath, u.df.filePath, logger)
	}
	if err != nil {
		// Not fatal: a transcript being appended to right now, or one the user
		// cannot read, must not abort the scan for every other session.
		logger.Warn("claude sessions: failed to read transcript",
			"file", u.df.filePath, "error", err)
		res.summary = nil
	}
	return res
}

// writeBatches drains results into transactions of up to scanBatchSize files.
func writeBatches(
	db *sql.DB, logger *slog.Logger, results <-chan scanResult, total int,
	pending map[string]pendingNotify, progress func(done, total int),
) {
	batch := make([]scanResult, 0, scanBatchSize)
	done := 0
	report := func() {
		if progress != nil {
			progress(done, total)
		}
	}
	report()

	flush := func() {
		if len(batch) == 0 {
			return
		}
		writeBatch(db, logger, batch, pending)
		done += len(batch)
		batch = batch[:0]
		report()
	}

	for res := range results {
		if res.summary == nil {
			done++
			report()
			continue
		}
		batch = append(batch, res)
		if len(batch) >= scanBatchSize {
			flush()
		}
	}
	flush()
}

// writeBatch commits one batch of decoded transcripts.
//
// A batch that fails to commit is logged and dropped rather than retried: the
// cached rows carry each file's mtime, so a file whose row did not commit still
// looks changed to the next diff and is re-read then. Retrying here would risk
// looping on a persistent error while the user waits.
func writeBatch(db *sql.DB, logger *slog.Logger, batch []scanResult, pending map[string]pendingNotify) {
	if err := runBatchTx(db, batch); err != nil {
		logger.Warn("claude sessions: failed to commit scan batch",
			"files", len(batch), "error", err)
		return
	}
	for _, res := range batch {
		recordPending(pending, res.unit)
	}
}

func runBatchTx(db *sql.DB, batch []scanResult) (err error) {
	ctx := context.Background()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			if rbErr := tx.Rollback(); rbErr != nil && !errors.Is(rbErr, sql.ErrTxDone) {
				err = errors.Join(err, rbErr)
			}
		}
	}()

	for _, res := range batch {
		if err = writeResult(ctx, tx, res); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// writeResult persists one decoded transcript inside the batch's transaction.
func writeResult(ctx context.Context, tx *sql.Tx, res scanResult) error {
	if res.unit.df.isSubagent {
		return upsertSubagentRow(ctx, tx, res.unit.df, res.summary, res.meta)
	}
	// The session row and its linked pull requests go together: the row carries
	// the file's mtime, so a PR write failing after the row committed would
	// leave the file looking unchanged to the next diff and the PR rows would
	// never be rebuilt.
	if err := insertCacheRow(ctx, tx, res.unit.df, res.summary); err != nil {
		return err
	}
	return replacePRRows(ctx, tx, res.summary.SessionID, res.summary.PRs)
}

// pendingNotify is one session's queued insight notification for this scan.
type pendingNotify struct {
	filePath string
	isNew    bool
}

// recordPending queues the session's insight notification.
//
// A sub-agent file is recorded against its PARENT session id and file path,
// because a changed fragment must re-run the whole session. It never marks the
// session as new — the session already existed — and never overwrites an entry
// the parent file recorded, so a genuinely new session still reports as new
// regardless of the order the two were written in.
func recordPending(pending map[string]pendingNotify, u scanUnit) {
	if u.df.isSubagent {
		if _, exists := pending[u.df.sessionID]; !exists {
			pending[u.df.sessionID] = pendingNotify{filePath: u.df.parentFilePath, isNew: false}
		}
		return
	}
	pending[u.df.sessionID] = pendingNotify{filePath: u.df.filePath, isNew: u.isNew}
}

// deleteCachedFiles removes the cache rows of transcripts no longer on disk,
// in one transaction rather than one per file.
func deleteCachedFiles(db *sql.DB, logger *slog.Logger, gone []cachedEntry) {
	if len(gone) == 0 {
		return
	}
	ctx := context.Background()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		logger.Warn("claude sessions: failed to begin delete transaction", "error", err)
		return
	}
	for _, ce := range gone {
		if err := deleteCachedFileTx(ctx, tx, ce); err != nil {
			logger.Warn("claude sessions: failed to delete cache row",
				"file", ce.filePath, "error", err)
			if rbErr := tx.Rollback(); rbErr != nil && !errors.Is(rbErr, sql.ErrTxDone) {
				logger.Warn("claude sessions: failed to roll back deletes", "error", rbErr)
			}
			return
		}
	}
	if err := tx.Commit(); err != nil {
		logger.Warn("claude sessions: failed to commit deletes", "error", err)
	}
}

// deleteCachedFileTx removes every cache row belonging to one removed
// transcript.
func deleteCachedFileTx(ctx context.Context, tx *sql.Tx, ce cachedEntry) error {
	table := "claude_session_cache"
	if ce.isSubagent {
		table = "claude_subagent_cache"
	} else {
		// Linked PRs hang off the session row with no foreign key, so they must
		// be cleared here or they outlive the session forever — and the list's
		// PR attach reads them back. This runs before the session row is
		// deleted, because it resolves the session through it.
		if _, err := tx.ExecContext(ctx,
			`DELETE FROM claude_session_pr WHERE session_id IN (
				SELECT session_id FROM claude_session_cache WHERE file_path = ?)`,
			ce.filePath); err != nil {
			return err
		}
	}
	// #nosec G202 -- table is a package-internal constant, never user input.
	_, err := tx.ExecContext(ctx, "DELETE FROM "+table+" WHERE file_path = ?", ce.filePath)
	return err
}
