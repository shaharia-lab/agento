package claudesessions

import "testing"

// The desktop shell owns the scan (#289) and starts the sidecar with
// AGENTO_SCANNER=off. Getting the default wrong in either direction is bad in a
// different way: on-by-mistake gives two writers on one SQLite file, and
// off-by-mistake means nothing scans at all and the corpus silently stops
// updating.
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
