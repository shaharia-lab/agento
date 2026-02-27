import AgentForm from '@/components/AgentForm'

export default function AgentCreatePage() {
  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-border px-4 sm:px-6 py-4">
        <h1 className="text-lg font-semibold">New Agent</h1>
        <p className="text-sm text-muted-foreground">
          Define a new AI agent with a custom system prompt and capabilities.
        </p>
      </div>
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        <AgentForm />
      </div>
    </div>
  )
}
