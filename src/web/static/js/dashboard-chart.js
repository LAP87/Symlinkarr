/* Dashboard scan-history chart. Reads the JSON data block rendered by the
 * template and draws a single-color, theme-aware line + area chart. All colors
 * come from CSS classes defined in style.css, so themes restyle the chart. */
(function () {
    "use strict";

    function drawChart() {
        var dataEl = document.getElementById("recent-runs-data");
        if (!dataEl) return;

        var data = [];
        try {
            data = JSON.parse(dataEl.textContent).reverse();
        } catch (e) {
            console.error("Failed to parse recent runs data", e);
            return;
        }

        var container = document.querySelector(".scan-history-chart-container");
        var emptyState = document.getElementById("scan-history-empty");

        if (data.length < 2) {
            if (container) container.hidden = true;
            if (emptyState) emptyState.hidden = false;
            return;
        }
        if (container) container.hidden = false;
        if (emptyState) emptyState.hidden = true;

        var svg = document.getElementById("scan-history-svg");
        var line = document.getElementById("chart-line");
        var area = document.getElementById("chart-area");
        var dotsContainer = document.getElementById("chart-dots");
        if (!svg || !line || !area || !dotsContainer) return;

        var width = 1000;
        var height = 240;
        var paddingX = 40;
        var paddingY = 30;

        var maxVal = Math.max.apply(null, data.map(function (d) {
            return Math.max(d.links, d.matches, 10);
        }));
        maxVal = Math.ceil(maxVal * 1.15); // add 15% headroom

        var points = data.map(function (d, i) {
            var x = paddingX + (i / (data.length - 1)) * (width - paddingX * 2);
            var y = height - paddingY - (d.links / maxVal) * (height - paddingY * 2);
            var dateStr = d.started_at;
            if (dateStr.length > 16) {
                dateStr = dateStr.substring(5, 16);
            }
            return { x: x, y: y, label: dateStr, value: d.links };
        });

        var pathD = "M " + points[0].x + "," + points[0].y;
        for (var i = 1; i < points.length; i++) {
            var cpX1 = points[i - 1].x + (points[i].x - points[i - 1].x) / 3;
            var cpY1 = points[i - 1].y;
            var cpX2 = points[i - 1].x + 2 * (points[i].x - points[i - 1].x) / 3;
            var cpY2 = points[i].y;
            pathD += " C " + cpX1 + "," + cpY1 + " " + cpX2 + "," + cpY2 + " " + points[i].x + "," + points[i].y;
        }

        line.setAttribute("d", pathD);

        var areaD = pathD
            + " L " + points[points.length - 1].x + "," + (height - paddingY)
            + " L " + points[0].x + "," + (height - paddingY)
            + " Z";
        area.setAttribute("d", areaD);

        dotsContainer.innerHTML = "";
        points.forEach(function (pt) {
            var g = document.createElementNS("http://www.w3.org/2000/svg", "g");
            g.setAttribute("class", "chart-dot");

            var circleOuter = document.createElementNS("http://www.w3.org/2000/svg", "circle");
            circleOuter.setAttribute("cx", pt.x);
            circleOuter.setAttribute("cy", pt.y);
            circleOuter.setAttribute("r", 9);
            circleOuter.setAttribute("class", "chart-dot__halo");

            var circleInner = document.createElementNS("http://www.w3.org/2000/svg", "circle");
            circleInner.setAttribute("cx", pt.x);
            circleInner.setAttribute("cy", pt.y);
            circleInner.setAttribute("r", 4.5);
            circleInner.setAttribute("class", "chart-dot__core");

            var title = document.createElementNS("http://www.w3.org/2000/svg", "title");
            title.textContent = pt.label + " | " + pt.value + " links created";

            g.appendChild(title);
            g.appendChild(circleOuter);
            g.appendChild(circleInner);
            dotsContainer.appendChild(g);
        });
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", drawChart);
    } else {
        drawChart();
    }

    document.body.addEventListener("htmx:afterSwap", function (evt) {
        if (evt.detail.target && evt.detail.target.id === "dashboard-summary") {
            drawChart();
        }
    });
})();
