package main

import "github.com/shaharia-lab/agento/cmd"

func main() {
	webFS, _ := getFrontendFS()
	cmd.WebFS = webFS
	cmd.Execute()
}
