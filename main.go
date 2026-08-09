package main

import (
	"log"
	// Embeds the IANA timezone database so time.LoadLocation works in a
	// distroless or scratch container, which has no /usr/share/zoneinfo.
	// Analytics buckets in the browser's timezone, so a lookup failure would
	// silently fall the whole dashboard back to UTC — the bug being fixed.
	// Costs roughly 450KB, which is the right trade for a single self-contained
	// binary.
	_ "time/tzdata"

	"github.com/shaharia-lab/agento/cmd"
)

func main() {
	webFS, err := getFrontendFS()
	if err != nil {
		log.Fatalf("failed to load frontend assets: %v", err)
	}
	cmd.WebFS = webFS
	cmd.Execute()
}
