/* Symlinkarr - Theme Switcher with built-in *arr portfolio */

class ThemeManager {
    constructor() {
        var manifest = window.SYMLINKARR_THEME_MANIFEST || { themes: [], aliases: {} };
        this.themes = manifest.themes || [];
        this.aliases = manifest.aliases || {};
        this.currentTheme = this.normalizeThemeId(this.loadTheme()) || 'auto';
        this.themeLink = null;
        this.colorSchemeQuery = null;
        this.init();
    }

    normalizeThemeId(themeId) {
        if (!themeId) {
            return null;
        }
        return this.aliases[themeId] || themeId;
    }

    getTheme(themeId) {
        return this.themes.find(function(theme) {
            return theme.id === themeId;
        }) || null;
    }

    prefersDarkMode() {
        return !!(window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
    }

    resolveTheme(themeId) {
        var selectionId = this.normalizeThemeId(themeId) || 'auto';
        var actualId = selectionId === 'auto'
            ? (this.prefersDarkMode() ? 'sonarr-dark' : 'sonarr-light')
            : selectionId;
        var selection = this.getTheme(selectionId) || this.getTheme('auto');
        var actual = this.getTheme(actualId) || this.getTheme('sonarr-dark');

        return {
            selectionId: selection.id,
            selection: selection,
            actual: actual,
        };
    }

    init() {
        this.themeLink = document.getElementById('theme-stylesheet');
        if (!this.themeLink) {
            this.themeLink = document.createElement('link');
            this.themeLink.rel = 'stylesheet';
            this.themeLink.id = 'theme-stylesheet';
            document.head.appendChild(this.themeLink);
        }

        this.colorSchemeQuery = window.matchMedia
            ? window.matchMedia('(prefers-color-scheme: dark)')
            : null;
        if (this.colorSchemeQuery) {
            var self = this;
            var handleChange = function() {
                if (self.currentTheme === 'auto') {
                    self.applyTheme('auto');
                }
            };
            if (this.colorSchemeQuery.addEventListener) {
                this.colorSchemeQuery.addEventListener('change', handleChange);
            } else if (this.colorSchemeQuery.addListener) {
                this.colorSchemeQuery.addListener(handleChange);
            }
        }

        this.applyTheme(this.currentTheme);
    }

    applyTheme(themeId) {
        var resolved = this.resolveTheme(themeId);
        var displayName = resolved.selectionId === 'auto'
            ? resolved.selection.name + ' (' + resolved.actual.name + ')'
            : resolved.actual.name;

        this.currentTheme = resolved.selectionId;
        this.themeLink.href = '/static/themes/' + resolved.actual.file;
        document.documentElement.setAttribute('data-theme', resolved.actual.id);
        document.documentElement.setAttribute('data-theme-selection', resolved.selectionId);
        this.saveTheme(resolved.selectionId);
        this.updatePicker();

        var toggle = document.getElementById('theme-picker-toggle');
        if (toggle) {
            toggle.setAttribute('aria-label', 'Choose theme (' + displayName + ')');
            toggle.setAttribute('title', 'Theme: ' + displayName);
        }
    }

    loadTheme() {
        try {
            return localStorage.getItem('symlinkarr-theme');
        } catch(e) {
            return null;
        }
    }

    saveTheme(themeId) {
        try {
            localStorage.setItem('symlinkarr-theme', themeId);
        } catch(e) {}
    }

    buildPicker(container) {
        var self = this;
        container.innerHTML = '';
        this.themes.forEach(function(theme) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'theme-option' + (self.currentTheme === theme.id ? ' active' : '');
            btn.setAttribute('data-theme', theme.id);
            btn.setAttribute('aria-pressed', self.currentTheme === theme.id ? 'true' : 'false');

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
                self.setDropdownOpen(false);
            });
            container.appendChild(btn);
        });
    }

    updatePicker() {
        var dropdown = document.getElementById('theme-picker-dropdown');
        if (dropdown) {
            this.buildPicker(dropdown);
        }
    }

    setDropdownOpen(isOpen) {
        var dropdown = document.getElementById('theme-picker-dropdown');
        var toggle = document.getElementById('theme-picker-toggle');
        if (dropdown) {
            dropdown.style.display = isOpen ? 'block' : 'none';
        }
        if (toggle) {
            toggle.setAttribute('aria-expanded', isOpen ? 'true' : 'false');
        }
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
            window.themeManager.setDropdownOpen(!isOpen);
        });

        document.addEventListener('click', function(e) {
            if (!e.target.closest('.theme-picker')) {
                window.themeManager.setDropdownOpen(false);
            }
        });
    }
});
