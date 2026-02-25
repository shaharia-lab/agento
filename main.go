package main

import "github.com/shaharia-lab/agents-platform-cc-go/cmd"

func main() {
	webFS, _ := getFrontendFS()
	cmd.WebFS = webFS
	cmd.Execute()
}
