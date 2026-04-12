/* Symlinkarr UI preferences */

(function () {
    function loadAdvancedPreference() {
        try {
            return localStorage.getItem("symlinkarr-advanced") === "1";
        } catch (e) {
            return false;
        }
    }

    function saveAdvancedPreference(enabled) {
        try {
            localStorage.setItem("symlinkarr-advanced", enabled ? "1" : "0");
        } catch (e) {}
    }

    function applyAdvancedPreference(enabled, toggle) {
        document.documentElement.classList.toggle("show-advanced", enabled);
        if (document.body) {
            document.body.classList.toggle("show-advanced", enabled);
        }

        if (!toggle) {
            return;
        }

        var label = toggle.querySelector(".pref-toggle__label");
        var nextLabel = enabled
            ? toggle.getAttribute("data-label-on")
            : toggle.getAttribute("data-label-off");

        toggle.setAttribute("aria-pressed", enabled ? "true" : "false");
        toggle.classList.toggle("pref-toggle--active", enabled);
        if (label && nextLabel) {
            label.textContent = nextLabel;
        }
    }

    document.addEventListener("DOMContentLoaded", function () {
        var toggle = document.getElementById("advanced-toggle");
        var enabled = loadAdvancedPreference();

        if (document.body && document.body.classList.contains("page-requires-advanced")) {
            enabled = true;
            saveAdvancedPreference(true);
        }

        applyAdvancedPreference(enabled, toggle);

        if (!toggle) {
            return;
        }

        toggle.addEventListener("click", function () {
            enabled = !document.documentElement.classList.contains("show-advanced");
            saveAdvancedPreference(enabled);
            applyAdvancedPreference(enabled, toggle);
        });
    });
}());
