package claudesessions

import "testing"

// TestFallbackPreviewLabel covers the rows that read as machine output in the
// sessions list. These previews are the last resort in ResolveDisplayTitle, so
// for a session that is only a slash command they are the whole label.
func TestFallbackPreviewLabel(t *testing.T) {
	cases := []struct {
		name string
		raw  string
		want string
	}{
		{
			name: "slash command becomes its own name",
			raw: "<command-message>lab-workflow:github-issue-to-pr</command-message>" +
				"<command-name>lab-workflow:github-issue-to-pr</command-name>" +
				"<command-args>306</command-args>",
			want: "/lab-workflow:github-issue-to-pr",
		},
		{
			name: "skill preamble becomes the skill name",
			raw:  "Base directory for this skill: /home/user/.claude/plugins/cache/lab/0.11.0/skills/github-issue-to-pr\n\n# GitHub Issue → PR",
			want: "skill: github-issue-to-pr",
		},
		{
			name: "a skill path with no skills segment falls back to its last one",
			raw:  "Base directory for this skill: /opt/tools/my-skill",
			want: "skill: my-skill",
		},
		{
			name: "anything else is left alone",
			raw:  "<system-reminder>The task list is empty</system-reminder>",
			want: "<system-reminder>The task list is empty</system-reminder>",
		},
		{
			// A person writing *about* a command must not be relabeled: the
			// pattern matches the tag Claude Code emits, not the prose.
			name: "prose mentioning a command is untouched",
			raw:  "why does command-name keep showing up in my titles?",
			want: "why does command-name keep showing up in my titles?",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := fallbackPreviewLabel(tc.raw); got != tc.want {
				t.Errorf("fallbackPreviewLabel(%q)\n got %q\nwant %q", tc.raw, got, tc.want)
			}
		})
	}
}

// TestSkillNameFromPath covers the path shapes Claude Code lays skills out in.
func TestSkillNameFromPath(t *testing.T) {
	for path, want := range map[string]string{
		"/home/u/.claude/plugins/cache/lab/0.11.0/skills/deploy": "deploy",
		"/home/u/.claude/skills/deploy/nested/dir":               "deploy",
		"/opt/skills": "skills",
		"deploy":      "deploy",
		"":            "",
	} {
		if got := skillNameFromPath(path); got != want {
			t.Errorf("skillNameFromPath(%q) = %q, want %q", path, got, want)
		}
	}
}
