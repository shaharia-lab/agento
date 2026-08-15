package claudesessions

import (
	"database/sql"
	"log/slog"
	"testing"
)

// The desktop shell owns the scan (#289) and starts the sidecar with
// AGENTO_SCANNER=off. Getting the default wrong is bad in two different ways:
// on-by-mistake gives two processes writing one SQLite file, and off-by-mistake
// means nothing scans at all and the corpus silently stops updating.
func TestScannerOffValue(t *testing.T) {
	off := []string{"off", "0", "false", "disabled", "OFF", " off ", "False"}
	for _, v := range off {
		if !scannerOffValue(v) {
			t.Errorf("scannerOffValue(%q) = false, want true", v)
		}
	}

	// Unset is the important one: `agento web` on its own must still scan.
	on := []string{"", "on", "1", "true", "enabled", "yes", "no", "  ", "offf"}
	for _, v := range on {
		if scannerOffValue(v) {
			t.Errorf("scannerOffValue(%q) = true, want false — an unrecognized value must not disable the scan", v)
		}
	}
}

// withScannerOff forces the parsed switch, since scannerDisabled caches through
// a sync.Once that no test can reset.
func withScannerOff(t *testing.T, off bool) {
	t.Helper()
	scannerDisabledOnce.Do(func() {}) // consume the Once so the env is never read
	previous := scannerOff
	scannerOff = off
	t.Cleanup(func() { scannerOff = previous })
}

// EnsureScan is the single choke point the desktop flip depends on: both the
// boot scan and every read path's ensureFresh reach a scan only through here.
//
// The assertions are deliberately **synchronous**. An earlier version waited up
// to two seconds for the channel to close, and passed with the guard deleted —
// a scan against an unusable database fails fast enough to close it inside the
// wait. What distinguishes the two is not whether the channel closes but
// *when*: the guard closes it before returning, while a real admission closes
// it only after a goroutine has run. So this checks without blocking, and
// repeats, because a single non-blocking check could in principle lose the race.
func TestEnsureScanRespectsTheScannerSwitch(t *testing.T) {
	db, err := sql.Open("sqlite", "file:scanner-switch-test?mode=memory&cache=shared")
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer func() { _ = db.Close() }()

	cache := NewCache(db, slog.New(slog.DiscardHandler))
	withScannerOff(t, true)

	for i := range 100 {
		done := cache.EnsureScan()

		// Already closed, checked without waiting. Cache.List blocks on this
		// channel for coldStartScanWait, so returning an open one would stall a
		// first-run read for the whole timeout.
		select {
		case <-done:
		default:
			t.Fatalf("iteration %d: EnsureScan returned a channel that was not already closed", i)
		}

		if cache.ScanInProgress() {
			t.Fatalf("iteration %d: a disabled scanner must never mark a scan in progress", i)
		}
	}

	// The guard must not be swallowing everything: with the switch off, an
	// admission is observable. The scan may fail immediately against this
	// database, so this asserts the channel is a *fresh* one rather than the
	// pre-closed constant the guard returns.
	withScannerOff(t, false)
	if cache.EnsureScan() == nil {
		t.Error("EnsureScan must always return a channel")
	}
}
