/* Generic sticky section rail: scroll-spy + click active state.
 *
 * Usage (any page, CSP-safe — no inline JS):
 *   <nav class="settings-rail" data-section-rail aria-label="Sections"> ... </nav>
 *   <a href="#some-section" class="settings-rail__link active">Label</a>
 * and load with:
 *   <script src="/static/js/section-rail.js" defer></script>
 *
 * config.html keeps its own config-rail.js; this file deliberately
 * only binds elements carrying the data-section-rail attribute.
 */
(function () {
    "use strict";

    function bindRail(rail) {
        var links = Array.prototype.slice.call(
            rail.querySelectorAll(".settings-rail__link")
        );
        if (links.length === 0) return;

        function setActive(id) {
            links.forEach(function (link) {
                link.classList.toggle("active", link.getAttribute("href") === "#" + id);
            });
        }

        function sections() {
            return links
                .map(function (link) {
                    var href = link.getAttribute("href") || "";
                    if (href.charAt(0) !== "#" || href.length < 2) return null;
                    return document.getElementById(href.slice(1));
                })
                .filter(Boolean);
        }

        function updateSpy() {
            var scrollPos = window.scrollY || document.documentElement.scrollTop || 0;
            var found = sections();
            var activeId = null;
            found.forEach(function (sec) {
                if (sec.offsetTop - 140 <= scrollPos) {
                    activeId = sec.getAttribute("id");
                }
            });
            if (!activeId && found.length > 0) {
                activeId = found[0].getAttribute("id");
            }
            if (activeId) setActive(activeId);
        }

        links.forEach(function (link) {
            link.addEventListener("click", function () {
                var href = link.getAttribute("href") || "";
                if (href.charAt(0) === "#") setActive(href.slice(1));
            });
        });

        window.addEventListener("scroll", updateSpy, { passive: true });
        window.addEventListener("resize", updateSpy);
        updateSpy();
    }

    function bind() {
        Array.prototype.slice
            .call(document.querySelectorAll("[data-section-rail]"))
            .forEach(bindRail);
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", bind);
    } else {
        bind();
    }
})();
