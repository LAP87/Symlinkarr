/* Config page: sticky section rail active-state (scroll spy + click). */
(function () {
    "use strict";

    function bind() {
        var rail = document.getElementById("config-rail");
        if (!rail) return;
        var links = Array.prototype.slice.call(rail.querySelectorAll(".settings-rail__link"));
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
                    return document.getElementById(href.slice(1));
                })
                .filter(Boolean);
        }

        function updateSpy() {
            var scrollPos = window.scrollY || document.documentElement.scrollTop || 0;
            var activeId = null;
            sections().forEach(function (sec) {
                if (sec.offsetTop - 140 <= scrollPos) {
                    activeId = sec.getAttribute("id");
                }
            });
            if (!activeId) {
                activeId = sections().length > 0 ? sections()[0].getAttribute("id") : null;
            }
            if (activeId) setActive(activeId);
        }

        links.forEach(function (link) {
            link.addEventListener("click", function () {
                var href = link.getAttribute("href") || "";
                setActive(href.slice(1));
            });
        });

        window.addEventListener("scroll", updateSpy, { passive: true });
        window.addEventListener("resize", updateSpy);
        updateSpy();
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", bind);
    } else {
        bind();
    }
})();
