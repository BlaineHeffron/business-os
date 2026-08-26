// Theme (light/dark) preference. Light is the product default; dark is opt-in
// and persisted as the only allowed browser storage preference. The class lives
// on <html> so the CSS token remap in index.css can swap the whole palette
// without per-component work.

export type Theme = "light" | "dark";

const STORAGE_KEY = "bos-theme";

export function getStoredTheme(): Theme {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v === "dark" ? "dark" : "light";
  } catch {
    return "light";
  }
}

export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  root.classList.toggle("dark", theme === "dark");
}

export function setTheme(theme: Theme): void {
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // Private mode / storage disabled: still apply for this session.
  }
  applyTheme(theme);
}

// Apply the stored preference as early as possible, before React renders.
export function initTheme(): Theme {
  const theme = getStoredTheme();
  applyTheme(theme);
  return theme;
}
