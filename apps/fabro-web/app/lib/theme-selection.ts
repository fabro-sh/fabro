type Theme = "light" | "dark";

const STORAGE_KEY = "fabro-theme";

function resolveTheme(storedTheme: string | null): Theme {
  return storedTheme === "light" || storedTheme === "dark"
    ? storedTheme
    : "dark";
}

function buildThemeBootScript(storageKey = STORAGE_KEY) {
  return `(function(){try{var t=localStorage.getItem("${storageKey}");if(t!=="light"&&t!=="dark")t="dark";document.documentElement.classList.add(t)}catch(e){document.documentElement.classList.add("dark")}})()`;
}

export { STORAGE_KEY, buildThemeBootScript, resolveTheme };
export type { Theme };
