/* Scan page: live filtering of saved anime search overrides. */
(function () {
    "use strict";

    function filterOverrides(query) {
        var term = query.toLowerCase().trim();
        var items = document.querySelectorAll(".override-item");
        var visible = 0;
        items.forEach(function (el) {
            var terms = el.getAttribute("data-terms") || "";
            var show = !term || terms.toLowerCase().indexOf(term) !== -1;
            el.style.display = show ? "" : "none";
            if (show) visible++;
        });
        var badge = document.getElementById("override-count-badge");
        if (badge) badge.textContent = visible;
    }

    function bind() {
        var input = document.getElementById("override-filter");
        if (!input) return;
        input.addEventListener("input", function () {
            filterOverrides(input.value);
        });
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", bind);
    } else {
        bind();
    }
})();
