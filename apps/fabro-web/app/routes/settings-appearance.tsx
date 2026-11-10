import { useAppearance } from "../components/appearance-provider";
import { DEFAULT_THEME, DISPLAY_MODES, THEMES, themeInfo } from "../lib/appearance";

export function meta() {
  return [{ title: "Appearance — Fabro" }];
}

/**
 * Factory's Appearance chooser: a palette card per theme and a separate display mode.
 * Both groups are native radios, which brings the roving tab stop and arrow keys.
 */
export default function SettingsAppearance() {
  const { theme, displayMode, appearance, saveStatus, setTheme, setDisplayMode, resetAppearance } =
    useAppearance();

  return (
    <div className="max-w-3xl">
      <p className="mb-3 text-[13px] text-fg-muted">
        Brutalist in light mode is the default look. Every palette keeps statuses and connectors readable in
        light and dark. This is a preference of this browser only; it never changes Fabro data.
      </p>

      <fieldset className="min-w-0">
        <legend className="mb-1 block text-xs font-bold text-fg-2">Palette</legend>
        <div className="grid gap-2 sm:grid-cols-2" data-appearance-themes>
          {THEMES.map((candidate) => (
            <label
              key={candidate.id}
              className="theme-choice"
              htmlFor={`appearance-theme-${candidate.id}`}
              data-theme-choice={candidate.id}
            >
              <input
                type="radio"
                id={`appearance-theme-${candidate.id}`}
                name="appearance-theme"
                value={candidate.id}
                checked={theme === candidate.id}
                onChange={() => setTheme(candidate.id)}
                className="mt-0.5 accent-[var(--accent-fill)]"
              />
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-1" aria-hidden="true">
                  {candidate.swatches.map((color, index) => (
                    <span key={index} className="theme-swatch" style={{ backgroundColor: color }} />
                  ))}
                </span>
                <span className="mt-1 flex flex-wrap items-center gap-1.5 text-[13px] font-extrabold text-fg">
                  {candidate.name}
                  {candidate.id === DEFAULT_THEME ? (
                    <span className="inline-flex h-[22px] items-center whitespace-nowrap rounded-[var(--radius-chip)] border border-line-strong px-2 text-[11.5px] font-bold leading-none text-fg-muted">
                      Default
                    </span>
                  ) : null}
                </span>
                <span className="block text-xs font-semibold text-fg-muted">
                  {candidate.description}
                </span>
              </span>
            </label>
          ))}
        </div>
      </fieldset>

      <fieldset className="mt-4 min-w-0">
        <legend className="mb-1 block text-xs font-bold text-fg-2">Display mode</legend>
        <div className="flex gap-2">
          {DISPLAY_MODES.map((candidate) => (
            <label key={candidate.id} className="group flex-1 cursor-pointer" data-mode-choice={candidate.id}>
              <input
                type="radio"
                name="appearance-mode"
                value={candidate.id}
                checked={displayMode === candidate.id}
                onChange={() => setDisplayMode(candidate.id)}
                className="peer sr-only"
              />
              <span className="flex h-[34px] items-center justify-center whitespace-nowrap rounded-[var(--radius-btn)] border border-line-strong bg-panel px-3 text-[13px] font-bold text-fg shadow-[var(--btn-shadow)] transition-colors group-hover:bg-panel-alt peer-checked:border-transparent peer-checked:bg-primary peer-checked:text-on-primary peer-checked:group-hover:bg-primary-hover peer-focus-visible:outline-2 peer-focus-visible:-outline-offset-2 peer-focus-visible:outline-current">
                {candidate.name}
              </span>
            </label>
          ))}
        </div>
      </fieldset>

      <p className="mt-3 text-[13px] font-semibold text-fg-2" role="status" data-appearance-status>
        {themeInfo(theme).name} · {appearance.mode}
        {displayMode === "system" ? ", following your device" : ""}.
        {saveStatus === "blocked" ? " This browser would not save it, so it lasts for this visit." : ""}
      </p>

      <button
        type="button"
        id="appearance-reset"
        onClick={() => resetAppearance()}
        className="mt-2 cursor-pointer text-[13px] font-bold text-fg underline decoration-[var(--accent-fill)] decoration-2 underline-offset-[3px] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus"
      >
        Reset to Brutalist, light mode
      </button>
    </div>
  );
}
