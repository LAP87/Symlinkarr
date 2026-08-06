/* Backup page: restore confirmation dialog wiring. */
(function () {
    "use strict";

    function bind() {
        var dialog = document.getElementById("restore-confirm-dialog");
        var restoreForm = document.getElementById("restore-confirm-form");
        var restoreInput = document.getElementById("restore-confirm-input");
        var confirmSubmit = document.getElementById("restore-confirm-submit");
        var triggers = document.querySelectorAll("[data-restore-trigger]");
        if (!restoreForm || !restoreInput) return;

        function setText(id, value) {
            var element = document.getElementById(id);
            if (element) {
                element.textContent = value;
            }
        }

        triggers.forEach(function (trigger) {
            trigger.addEventListener("click", function () {
                var snapshotSize = trigger.dataset.snapshotSizeBytes;
                var summary = [
                    "Restore backup " + trigger.dataset.backupFile + "?",
                    "",
                    "Type: " + trigger.dataset.backupKind,
                    "Created: " + trigger.dataset.createdAt + " (" + trigger.dataset.ageLabel + ")",
                    "Recorded links: " + trigger.dataset.recordedLinks + " (" + trigger.dataset.linkDeltaLabel + ")",
                    "Manifest size: " + trigger.dataset.manifestSizeBytes + " bytes",
                    "SQLite snapshot: " + (snapshotSize ? snapshotSize + " bytes" : "not included"),
                    "Config snapshot: " + (trigger.dataset.configSnapshotPresent === "yes" ? "included" : "not included"),
                    "Secret snapshots: " + trigger.dataset.secretSnapshotCount,
                ].join("\n");

                restoreInput.value = trigger.dataset.backupFile;
                setText("restore-confirm-file", trigger.dataset.backupFile);
                setText("restore-confirm-kind", trigger.dataset.backupKind);
                setText("restore-confirm-label", trigger.dataset.backupLabel);
                setText("restore-confirm-created", trigger.dataset.createdAt);
                setText("restore-confirm-age", trigger.dataset.ageLabel);
                setText("restore-confirm-links", trigger.dataset.recordedLinks);
                setText("restore-confirm-link-delta", trigger.dataset.linkDeltaLabel);
                setText("restore-confirm-manifest-size", trigger.dataset.manifestSizeBytes + " bytes");
                setText("restore-confirm-snapshot-size", snapshotSize ? snapshotSize + " bytes" : "Not included");
                setText("restore-confirm-config-snapshot", trigger.dataset.configSnapshotPresent === "yes" ? "Included" : "Not included");
                setText("restore-confirm-secret-snapshots", trigger.dataset.secretSnapshotCount);

                if (dialog && typeof dialog.showModal === "function") {
                    dialog.showModal();
                } else if (window.confirm(summary)) {
                    restoreForm.submit();
                }
            });
        });

        if (confirmSubmit) {
            confirmSubmit.addEventListener("click", function () {
                restoreForm.submit();
            });
        }
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", bind);
    } else {
        bind();
    }
})();
