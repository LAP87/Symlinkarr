/* Symlinkarr boot script.
 * Loaded synchronously in <head> right after theme-manifest.js so the theme
 * and advanced-mode preferences are applied before first paint (no flash).
 */

(function () {
    var manifest = window.SYMLINKARR_THEME_MANIFEST || null;

    if (!manifest || !manifest.resolveSelection || !manifest.buildThemeCss) {
        return;
    }

    function prefersDark() {
        return !!(window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
    }

    var storedTheme = null;
    try {
        storedTheme = localStorage.getItem("symlinkarr-theme");
    } catch (e) {}

    var resolved = manifest.resolveSelection(storedTheme, prefersDark());
    var style = document.getElementById("theme-vars");

    if (style && resolved.actual) {
        style.textContent = manifest.buildThemeCss(resolved.actual);
    }

    document.documentElement.setAttribute("data-theme-selection", resolved.selectionId);
    document.documentElement.setAttribute("data-theme", resolved.actual ? resolved.actual.id : "");
}());

(function () {
    try {
        if (localStorage.getItem("symlinkarr-advanced") === "1") {
            document.documentElement.classList.add("show-advanced");
        }
    } catch (e) {}
}());
