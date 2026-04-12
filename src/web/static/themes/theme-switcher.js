/* Symlinkarr - Theme Switcher with Picker */

class ThemeManager {
    constructor() {
        this.themes = [
            { id: 'dark',          name: 'Dark',          file: 'dark-theme.css',          swatches: ['#1a1d23','#21252b','#35c5f4'] },
            { id: 'light',         name: 'Light',         file: 'light-theme.css',         swatches: ['#ffffff','#f8f9fa','#007bff'] },
            { id: 'compact',       name: 'Compact',       file: 'compact-theme.css',       swatches: ['#1a1d23','#21252b','#35c5f4'] },
            { id: 'accessibility', name: 'High Contrast', file: 'accessibility-theme.css', swatches: ['#000000','#111111','#ffff00'] },
            { id: 'colorblind',    name: 'Colorblind',    file: 'colorblind-theme.css',    swatches: ['#1a1d23','#21252b','#0077cc'] },
        ];
        this.currentTheme = this.loadTheme() || 'dark';
        this.themeLink = null;
        this.init();
    }

    init() {
        this.themeLink = document.getElementById('theme-stylesheet');
        if (!this.themeLink) {
            this.themeLink = document.createElement('link');
            this.themeLink.rel = 'stylesheet';
            this.themeLink.id = 'theme-stylesheet';
            document.head.appendChild(this.themeLink);
        }
        this.applyTheme(this.currentTheme);
    }

    applyTheme(themeId) {
        var theme = this.themes.find(function(t) { return t.id === themeId; });
        if (!theme) {
            theme = this.themes[0];
            themeId = theme.id;
        }
        this.currentTheme = themeId;
        this.themeLink.href = '/static/themes/' + theme.file;
        this.saveTheme(themeId);
        this.updatePicker();
    }

    loadTheme() {
        try { return localStorage.getItem('symlinkarr-theme'); }
        catch(e) { return null; }
    }

    saveTheme(themeId) {
        try { localStorage.setItem('symlinkarr-theme', themeId); }
        catch(e) {}
    }

    buildPicker(container) {
        var self = this;
        container.innerHTML = '';
        this.themes.forEach(function(theme) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'theme-option' + (self.currentTheme === theme.id ? ' active' : '');
            btn.setAttribute('data-theme', theme.id);

            var swatchWrap = document.createElement('span');
            swatchWrap.className = 'theme-swatches';
            theme.swatches.forEach(function(color) {
                var swatch = document.createElement('span');
                swatch.style.background = color;
                swatchWrap.appendChild(swatch);
            });

            var label = document.createElement('span');
            label.textContent = theme.name;

            btn.appendChild(swatchWrap);
            btn.appendChild(label);
            btn.addEventListener('click', function() {
                self.applyTheme(theme.id);
                // Close dropdown after selection
                var dropdown = document.getElementById('theme-picker-dropdown');
                if (dropdown) dropdown.style.display = 'none';
            });
            container.appendChild(btn);
        });
    }

    updatePicker() {
        var dropdown = document.getElementById('theme-picker-dropdown');
        if (dropdown) this.buildPicker(dropdown);
    }
}

document.addEventListener('DOMContentLoaded', function() {
    window.themeManager = new ThemeManager();

    var dropdown = document.getElementById('theme-picker-dropdown');
    var toggle = document.getElementById('theme-picker-toggle');

    if (dropdown && toggle) {
        window.themeManager.buildPicker(dropdown);

        toggle.addEventListener('click', function(e) {
            e.stopPropagation();
            var isOpen = dropdown.style.display !== 'none';
            dropdown.style.display = isOpen ? 'none' : 'block';
        });

        document.addEventListener('click', function(e) {
            if (!e.target.closest('.theme-picker')) {
                dropdown.style.display = 'none';
            }
        });
    }
});
