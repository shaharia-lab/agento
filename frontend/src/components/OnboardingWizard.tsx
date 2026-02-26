import { useState } from 'react'
import { FolderOpen } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import FilesystemBrowserModal from '@/components/FilesystemBrowserModal'
import { settingsApi, filesystemApi } from '@/lib/api'
import { MODELS } from '@/types'

interface OnboardingWizardProps {
  defaultWorkingDir: string
  defaultModel: string
  onComplete: () => void
}

export default function OnboardingWizard({
  defaultWorkingDir,
  defaultModel,
  onComplete,
}: OnboardingWizardProps) {
  const [step, setStep] = useState(1)
  const [workingDir, setWorkingDir] = useState(defaultWorkingDir)
  const [model, setModel] = useState(defaultModel)
  const [browserOpen, setBrowserOpen] = useState(false)
  const [dirWarning, setDirWarning] = useState<string | null>(null)
  const [dirChecking, setDirChecking] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const checkDir = async (path: string): Promise<boolean> => {
    setDirChecking(true)
    setDirWarning(null)
    try {
      await filesystemApi.list(path)
      return true
    } catch {
      setDirWarning(`"${path}" does not exist. Create it?`)
      return false
    } finally {
      setDirChecking(false)
    }
  }

  const handleNext = async () => {
    const exists = await checkDir(workingDir)
    if (exists) setStep(2)
  }

  const handleCreateDir = async () => {
    try {
      await filesystemApi.mkdir(workingDir)
      setDirWarning(null)
      setStep(2)
    } catch {
      setError('Failed to create directory')
    }
  }

  const handleGetStarted = async () => {
    setSaving(true)
    setError(null)
    try {
      await settingsApi.update({
        default_working_dir: workingDir,
        default_model: model,
        onboarding_complete: true,
      })
      onComplete()
    } catch {
      setError('Failed to save settings. Please try again.')
    } finally {
      setSaving(false)
    }
  }

  return (
    <>
      {/* Full-screen overlay */}
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-white">
        <div className="w-full max-w-md px-6">
          {/* Step indicator */}
          <div className="flex items-center justify-center gap-2 mb-8">
            <StepDot active={step === 1} done={step > 1} label="1" />
            <div className="h-px w-8 bg-zinc-200" />
            <StepDot active={step === 2} done={false} label="2" />
          </div>

          {step === 1 && (
            <div className="flex flex-col gap-5">
              <div>
                <h1 className="text-2xl font-bold text-zinc-900 mb-2">Welcome to Agento</h1>
                <p className="text-sm text-zinc-500">Let's get you set up in two quick steps.</p>
              </div>

              <div>
                <h2 className="text-sm font-semibold text-zinc-900 mb-1">
                  Step 1 of 2 — Working Directory
                </h2>
                <p className="text-xs text-zinc-500 mb-3">
                  This is the directory where agents will run commands. You can change it per chat
                  session.
                </p>

                <div className="flex gap-2">
                  <Input
                    value={workingDir}
                    onChange={e => {
                      setWorkingDir(e.target.value)
                      setDirWarning(null)
                    }}
                    placeholder="/tmp/agento/work"
                    className="flex-1 font-mono text-sm"
                  />
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setBrowserOpen(true)}
                    className="shrink-0 gap-1.5"
                  >
                    <FolderOpen className="h-3.5 w-3.5" />
                    Browse
                  </Button>
                </div>

                {dirWarning && (
                  <div className="mt-2 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800">
                    <p>{dirWarning}</p>
                    <div className="flex gap-2 mt-2">
                      <Button
                        size="sm"
                        variant="outline"
                        className="h-7 text-xs"
                        onClick={handleCreateDir}
                      >
                        Yes, create it
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-7 text-xs"
                        onClick={() => setDirWarning(null)}
                      >
                        No
                      </Button>
                    </div>
                  </div>
                )}

                {error && <p className="mt-2 text-sm text-red-600">{error}</p>}
              </div>

              <Button
                className="bg-zinc-900 hover:bg-zinc-800 text-white w-full"
                onClick={() => void handleNext()}
                disabled={dirChecking || !workingDir}
              >
                {dirChecking ? 'Checking…' : 'Next'}
              </Button>
            </div>
          )}

          {step === 2 && (
            <div className="flex flex-col gap-5">
              <div>
                <h2 className="text-sm font-semibold text-zinc-900 mb-1">
                  Step 2 of 2 — Default AI Model
                </h2>
                <p className="text-xs text-zinc-500 mb-3">
                  Choose the default model for direct chats. Agents can have their own model
                  configured.
                </p>

                <Select value={model} onValueChange={setModel}>
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder="Select a model" />
                  </SelectTrigger>
                  <SelectContent>
                    {MODELS.map(m => (
                      <SelectItem key={m.value} value={m.value}>
                        {m.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              {error && <p className="text-sm text-red-600">{error}</p>}

              <div className="flex gap-2">
                <Button variant="outline" className="flex-1" onClick={() => setStep(1)}>
                  Back
                </Button>
                <Button
                  className="flex-1 bg-zinc-900 hover:bg-zinc-800 text-white"
                  onClick={() => void handleGetStarted()}
                  disabled={saving || !model}
                >
                  {saving ? 'Saving…' : 'Get Started'}
                </Button>
              </div>
            </div>
          )}
        </div>
      </div>

      <FilesystemBrowserModal
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        initialPath={workingDir}
        onSelect={path => {
          setWorkingDir(path)
          setDirWarning(null)
        }}
      />
    </>
  )
}

function StepDot({ active, done, label }: { active: boolean; done: boolean; label: string }) {
  return (
    <div
      className={[
        'flex h-7 w-7 items-center justify-center rounded-full text-xs font-semibold transition-colors',
        active
          ? 'bg-zinc-900 text-white'
          : done
            ? 'bg-zinc-300 text-zinc-600'
            : 'bg-zinc-100 text-zinc-400',
      ].join(' ')}
    >
      {label}
    </div>
  )
}
