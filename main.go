package main

import (
	"log"

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
