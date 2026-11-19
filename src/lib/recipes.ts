/* ============================================================================
   Task recipes (#698), through the shell for the file and through /api for
   the import.

   `src-tauri/src/native/recipes.rs` owns the format and what it leaves out.
   Export is one `invoke` that writes the picked path, `lib/sessionExport.ts`'s
   shape; import reads the picked file through a command and posts its text to
   `POST /api/tasks/import`, so the write goes through the same path as every
   other. Neither file dialog exists in a plain browser tab (`npm run dev`),
   so the view checks `RECIPES_AVAILABLE` rather than failing on click.
   ========================================================================== */

import { IS_TAURI } from "./tauri";
import type { TaskRecipe } from "./types";

export const RECIPES_AVAILABLE = IS_TAURI;

/** `{task-name-slug}.agento-recipe.json`, "task" standing in for no name. */
export function recipeFileName(name: string): string {
  const slug =
    name
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[̀-ͯ]/g, "")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 60)
      .replace(/-+$/, "") || "task";
  return `${slug}.agento-recipe.json`;
}

/** Native Save-As dialog; `null` when cancelled. `dialog:default` covers it. */
export async function pickRecipeSavePath(defaultName: string): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const picked = await save({
    title: "Export recipe",
    defaultPath: defaultName,
    filters: [{ name: "Agento recipe", extensions: ["json"] }],
  });
  return typeof picked === "string" ? picked : null;
}

/** Native Open dialog for one file; `null` when cancelled. */
export async function pickRecipeOpenPath(): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    title: "Import recipe",
    multiple: false,
    directory: false,
    filters: [{ name: "Agento recipe", extensions: ["json"] }],
  });
  return typeof picked === "string" ? picked : null;
}

/** Write task `taskId`'s recipe to `destPath`. Answers the bytes written. */
export async function exportTaskRecipe(taskId: string, destPath: string): Promise<number> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<number>("export_task_recipe", { taskId, destPath });
}

/** The picked recipe file's text. The command refuses one over 256 KiB. */
export async function readRecipeFile(path: string): Promise<string> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<string>("read_recipe_file", { path });
}

const DESTINATION_LABEL: Record<string, string> = {
  slack: "Slack destination",
  telegram: "Telegram destination",
  email: "Email destination",
};

const TRIGGER_LABEL: Record<string, string> = {
  slack: "Slack trigger",
  telegram: "Telegram trigger",
};

/**
 * What an import described and did not create, as labels for the "to
 * connect" note: every destination but `reply`, and every trigger. Read from
 * the file text the view already holds; an import the server accepted has
 * already been checked, so anything unreadable here is simply nothing.
 */
export function unconnectedWiring(text: string): string[] {
  let recipe: Partial<TaskRecipe>;
  try {
    recipe = JSON.parse(text) as Partial<TaskRecipe>;
  } catch {
    return [];
  }
  const out: string[] = [];
  for (const d of recipe.destinations ?? []) {
    const label = d && DESTINATION_LABEL[d.type];
    if (label) out.push(label);
  }
  for (const t of recipe.triggers ?? []) {
    const label = t && TRIGGER_LABEL[t.type];
    if (label) out.push(label);
  }
  return out;
}
