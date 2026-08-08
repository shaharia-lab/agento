package claudesessions

import "strings"

// mcpToolPrefix is how Claude Code names a tool exposed by an MCP server:
// mcp__<server>__<tool>.
const mcpToolPrefix = "mcp__"

// AttributionProcessor breaks tool usage down by the skill, plugin and MCP
// server responsible for it, turning an anonymous "N tool calls" into something
// a user can act on.
//
// Counting is per tool_use block, not per event. Claude Code splits one
// assistant message into several JSONL events — thinking, text, each tool call —
// which all share a message id and therefore carry identical attribution
// fields. Counting per event would inflate every number by a variable factor,
// and tool calls are the unit the breakdown is meant to explain anyway.
//
// MCP attribution comes from the tool_use block name rather than the
// attributionMcpServer/attributionMcpTool fields. Those fields hold the last
// MCP tool touched and persist onto later, unrelated turns: across the
// reference corpus only ~63 of ~730 tool-bearing events had them agree with the
// block actually being called. The block name is authoritative.
type AttributionProcessor struct {
	skills       map[string]int
	plugins      map[string]int
	mcpServers   map[string]int
	mcpTools     map[string]int
	efforts      map[string]int
	unattributed int
}

// Name returns the processor identifier.
func (p *AttributionProcessor) Name() string { return "attribution" }

// Process attributes each tool_use block in an assistant message to the skill,
// plugin, MCP server and effort tier of the event carrying it.
func (p *AttributionProcessor) Process(ev ProcessableEvent) {
	if ev.Message == nil || ev.Message.Role != "assistant" {
		return
	}
	p.ensureMaps()

	for _, b := range parseContentBlocks(ev.Message.Content) {
		if b.Type != "tool_use" || b.Name == "" {
			continue
		}
		if ev.AttributionSkill != "" {
			p.skills[ev.AttributionSkill]++
		} else {
			// Built-in tool use, with no skill's instructions in context.
			p.unattributed++
		}
		// A skill can be user-level rather than shipped by a plugin, so the
		// plugin is counted independently instead of nested under the skill.
		if ev.AttributionPlugin != "" {
			p.plugins[ev.AttributionPlugin]++
		}
		if ev.Effort != "" {
			p.efforts[ev.Effort]++
		}
		if server, tool, ok := splitMCPToolName(b.Name); ok {
			p.mcpServers[server]++
			p.mcpTools[tool]++
		}
	}
}

// splitMCPToolName splits an MCP tool call name of the form
// mcp__<server>__<tool> into its server and tool parts. It reports false for
// any name that is not an MCP tool call.
//
// Server names themselves may contain underscores (e.g.
// vibexp_io_vibexp_team), so the split is on the first "__" separator after the
// prefix rather than on every underscore.
func splitMCPToolName(name string) (server, tool string, ok bool) {
	rest, found := strings.CutPrefix(name, mcpToolPrefix)
	if !found {
		return "", "", false
	}
	server, tool, found = strings.Cut(rest, "__")
	if !found || server == "" || tool == "" {
		return "", "", false
	}
	return server, tool, true
}

// Finalize writes every breakdown into the insight.
func (p *AttributionProcessor) Finalize(insight *SessionInsight) {
	insight.SkillBreakdown = mergeCounts(insight.SkillBreakdown, p.skills)
	insight.PluginBreakdown = mergeCounts(insight.PluginBreakdown, p.plugins)
	insight.McpServerBreakdown = mergeCounts(insight.McpServerBreakdown, p.mcpServers)
	insight.McpToolBreakdown = mergeCounts(insight.McpToolBreakdown, p.mcpTools)
	insight.EffortBreakdown = mergeCounts(insight.EffortBreakdown, p.efforts)
	insight.UnattributedCalls = p.unattributed
}

// mergeCounts copies src into dst, allocating dst when absent so the field
// marshals to {} rather than null.
func mergeCounts(dst, src map[string]int) map[string]int {
	if dst == nil {
		dst = make(map[string]int, len(src))
	}
	for k, v := range src {
		dst[k] = v
	}
	return dst
}

// Reset clears all internal state.
func (p *AttributionProcessor) Reset() {
	p.skills = make(map[string]int)
	p.plugins = make(map[string]int)
	p.mcpServers = make(map[string]int)
	p.mcpTools = make(map[string]int)
	p.efforts = make(map[string]int)
	p.unattributed = 0
}

// ensureMaps allocates lazily, so the zero value is usable — the registry
// constructs processors with pre-made maps, but tests use the zero value.
func (p *AttributionProcessor) ensureMaps() {
	if p.skills == nil {
		p.Reset()
	}
}
