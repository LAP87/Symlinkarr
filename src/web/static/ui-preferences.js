/* Symlinkarr UI preferences */

(function () {
    function applyMobileNavState(open, toggle) {
        if (!document.body) {
            return;
        }

        document.body.classList.toggle("mobile-nav-open", open);

        if (!toggle) {
            return;
        }

        var label = toggle.querySelector(".sidebar-nav-toggle__label");
        var icon = toggle.querySelector(".sidebar-nav-toggle__icon");

        toggle.setAttribute("aria-expanded", open ? "true" : "false");

        if (label) {
            label.textContent = open ? "Close" : "Menu";
        }

        if (icon) {
            icon.textContent = open ? "✕" : "☰";
        }
    }

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
        var navToggle = document.getElementById("sidebar-nav-toggle");
        var mobileQuery = window.matchMedia
            ? window.matchMedia("(max-width: 768px)")
            : null;
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

        if (navToggle) {
            applyMobileNavState(false, navToggle);

            navToggle.addEventListener("click", function () {
                var open = !document.body.classList.contains("mobile-nav-open");
                applyMobileNavState(open, navToggle);
            });

            document.querySelectorAll(".nav-link").forEach(function (link) {
                link.addEventListener("click", function () {
                    if (mobileQuery && mobileQuery.matches) {
                        applyMobileNavState(false, navToggle);
                    }
                });
            });

            document.addEventListener("keydown", function (event) {
                if (event.key === "Escape") {
                    applyMobileNavState(false, navToggle);
                }
            });

            if (mobileQuery) {
                var syncMobileNav = function () {
                    if (!mobileQuery.matches) {
                        applyMobileNavState(false, navToggle);
                    }
                };

                if (mobileQuery.addEventListener) {
                    mobileQuery.addEventListener("change", syncMobileNav);
                } else if (mobileQuery.addListener) {
                    mobileQuery.addListener(syncMobileNav);
                }
            }
        }
    });
}());
