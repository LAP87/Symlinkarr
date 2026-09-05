/* Shared confirm-gate: a checkbox with data-enables="<button-id>" enables the
 * named button only while checked. Used by destructive apply forms. */
(function () {
    "use strict";

    function bind() {
        document.querySelectorAll('input[type="checkbox"][data-enables]').forEach(function (checkbox) {
            var button = document.getElementById(checkbox.getAttribute("data-enables"));
            if (!button) return;
            var sync = function () {
                button.disabled = !checkbox.checked;
            };
            checkbox.addEventListener("change", sync);
            sync();
        });
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", bind);
    } else {
        bind();
    }
})();
