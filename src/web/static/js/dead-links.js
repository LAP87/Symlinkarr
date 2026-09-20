/* Symlinkarr dead-links UI controller:
 * - Live background polling when repair or dead-link pruning is running
 * - Auto-reloads to show completed counters without manual refresh
 * - Destructive prune confirmation gate (skipped for dry runs)
 * - Operator feedback during mutation submissions
 */
(function () {
    "use strict";

    function initLivePolling() {
        var activeBanner = document.querySelector("[data-live-job]");
        if (!activeBanner) return;

        var jobKind = activeBanner.getAttribute("data-live-job");
        var pollUrl = "/api/v1/links/dead/status";
        var pollTimer = null;
        var consecutiveErrors = 0;

        function checkStatus() {
            fetch(pollUrl, {
                headers: { "Accept": "application/json" }
            })
            .then(function (res) {
                if (!res.ok) throw new Error("HTTP " + res.status);
                return res.json();
            })
            .then(function (data) {
                consecutiveErrors = 0;
                var isRunning = false;
                if (jobKind === "repair") {
                    isRunning = !!(data.active_repair);
                } else if (jobKind === "dead_prune") {
                    isRunning = !!(data.active_prune);
                } else {
                    isRunning = !!(data.active_repair || data.active_prune);
                }

                if (!isRunning) {
                    if (pollTimer) clearInterval(pollTimer);
                    var badge = activeBanner.querySelector("strong");
                    if (badge) {
                        badge.textContent = "Operation completed! Reloading view...";
                    }
                    setTimeout(function () {
                        window.location.reload();
                    }, 800);
                }
            })
            .catch(function () {
                consecutiveErrors++;
                if (consecutiveErrors > 10 && pollTimer) {
                    clearInterval(pollTimer);
                }
            });
        }

        pollTimer = setInterval(checkStatus, 2500);
    }

    function initPruneConfirmation() {
        var pruneForms = document.querySelectorAll('form[action="/links/dead/prune"]');
        pruneForms.forEach(function (form) {
            form.addEventListener("submit", function (e) {
                var dryRunBox = form.querySelector('input[name="dry_run"]');
                var isDryRun = dryRunBox && dryRunBox.checked;
                if (!isDryRun) {
                    var confirmed = window.confirm(
                        "Are you sure you want to permanently prune dead symlinks?\n\n" +
                        "• Broken symlinks on disk will be deleted.\n" +
                        "• Their database records will be marked removed.\n" +
                        "• Media server invalidation and Arr rescans will be triggered."
                    );
                    if (!confirmed) {
                        e.preventDefault();
                        return;
                    }
                }
                var btn = form.querySelector('button[type="submit"]');
                if (btn) {
                    btn.disabled = true;
                    btn.textContent = isDryRun ? "Starting dry run..." : "Starting prune...";
                }
            });
        });
    }

    function initActionFeedback() {
        var repairForms = document.querySelectorAll('form[action="/links/repair"]');
        repairForms.forEach(function (form) {
            form.addEventListener("submit", function () {
                var btn = form.querySelector('button[type="submit"]');
                if (btn) {
                    btn.disabled = true;
                    btn.textContent = "Starting repair...";
                }
            });
        });

        var exportForms = document.querySelectorAll('form[action="/links/dead/export-wanted"]');
        exportForms.forEach(function (form) {
            form.addEventListener("submit", function () {
                var btn = form.querySelector('button[type="submit"]');
                if (btn) {
                    btn.disabled = true;
                    btn.textContent = "Exporting...";
                }
            });
        });
    }

    function bind() {
        initLivePolling();
        initPruneConfirmation();
        initActionFeedback();
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", bind);
    } else {
        bind();
    }
})();
