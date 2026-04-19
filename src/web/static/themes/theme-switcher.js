/* Symlinkarr - Token-based theme switcher */

class ThemeManager {
    constructor() {
        this.manifest = window.SYMLINKARR_THEME_MANIFEST || {
            themes: [],
            aliases: {},
            defaults: { dark: 'symlinkarr-dark', light: 'symlinkarr-light' },
            resolveSelection: function () {
                return { selectionId: 'auto', selection: null, actual: null };
            },
            buildThemeCss: function () {
                return '';
            },
        };
        this.themes = this.manifest.themes || [];
        this.currentTheme = this.normalizeThemeId(this.loadTheme()) || 'auto';
        this.themeStyle = null;
        this.colorSchemeQuery = null;
        this.boundPositionDropdown = this.positionDropdown.bind(this);
        this.dropdownListenersBound = false;
        this.init();
    }

    normalizeThemeId(themeId) {
        if (!themeId) {
            return null;
        }
        return this.manifest.aliases[themeId] || themeId;
    }

    prefersDarkMode() {
        return !!(window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
    }

    resolveTheme(themeId) {
        return this.manifest.resolveSelection(themeId, this.prefersDarkMode());
    }

    ensureThemeStyle() {
        this.themeStyle = document.getElementById('theme-vars');
        if (!this.themeStyle) {
            this.themeStyle = document.createElement('style');
            this.themeStyle.id = 'theme-vars';
            document.head.appendChild(this.themeStyle);
        }
    }

    init() {
        this.ensureThemeStyle();

        this.colorSchemeQuery = window.matchMedia
            ? window.matchMedia('(prefers-color-scheme: dark)')
            : null;
        if (this.colorSchemeQuery) {
            var self = this;
            var handleChange = function () {
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
        if (!resolved.actual) {
            return;
        }

        var displayName = resolved.selectionId === 'auto'
            ? resolved.selection.name + ' (' + resolved.actual.name + ')'
            : resolved.actual.name;

        this.currentTheme = resolved.selectionId;
        this.themeStyle.textContent = this.manifest.buildThemeCss(resolved.actual);
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
        } catch (e) {
            return null;
        }
    }

    saveTheme(themeId) {
        try {
            localStorage.setItem('symlinkarr-theme', themeId);
        } catch (e) {}
    }

    buildPicker(container) {
        var self = this;

        container.innerHTML = '';
        this.themes.forEach(function (theme) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'theme-option' + (self.currentTheme === theme.id ? ' active' : '');
            btn.setAttribute('data-theme', theme.id);
            btn.setAttribute('aria-pressed', self.currentTheme === theme.id ? 'true' : 'false');

            var swatchWrap = document.createElement('span');
            swatchWrap.className = 'theme-swatches';
            (theme.swatches || []).forEach(function (color) {
                var swatch = document.createElement('span');
                swatch.style.background = color;
                swatchWrap.appendChild(swatch);
            });

            var label = document.createElement('span');
            label.className = 'theme-option__label';
            label.textContent = theme.name;

            btn.appendChild(swatchWrap);
            btn.appendChild(label);

            if (theme.mode === 'dark' || theme.mode === 'light') {
                var meta = document.createElement('span');
                meta.className = 'theme-option__meta';
                meta.textContent = theme.mode;
                btn.appendChild(meta);
            }

            btn.addEventListener('click', function () {
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
            if (dropdown.style.display !== 'none') {
                this.positionDropdown();
            }
        }
    }

    positionDropdown() {
        var dropdown = document.getElementById('theme-picker-dropdown');
        var toggle = document.getElementById('theme-picker-toggle');
        if (!dropdown || !toggle || dropdown.style.display === 'none') {
            return;
        }

        var viewportWidth = window.innerWidth || document.documentElement.clientWidth;
        var viewportHeight = window.innerHeight || document.documentElement.clientHeight;
        var margin = 12;
        var gap = 8;
        var preferredWidth = 288;
        var availableWidth = Math.max(0, viewportWidth - (margin * 2));
        var maxWidth = availableWidth;
        if (maxWidth < 220) {
            maxWidth = Math.max(120, availableWidth);
        }
        maxWidth = Math.min(maxWidth, availableWidth || preferredWidth);

        dropdown.style.width = Math.min(preferredWidth, maxWidth) + 'px';
        dropdown.style.maxWidth = maxWidth + 'px';
        dropdown.style.left = margin + 'px';
        dropdown.style.top = margin + 'px';

        var toggleRect = toggle.getBoundingClientRect();
        var dropdownRect = dropdown.getBoundingClientRect();
        var dropdownWidth = Math.min(dropdownRect.width || dropdown.offsetWidth || preferredWidth, maxWidth);
        var left = Math.min(
            Math.max(margin, toggleRect.right - dropdownWidth),
            viewportWidth - dropdownWidth - margin
        );

        var availableHeight = Math.max(0, viewportHeight - (margin * 2));
        var maxHeight = Math.min(544, availableHeight);
        if (maxHeight < 180) {
            maxHeight = Math.max(120, availableHeight);
        }
        maxHeight = Math.min(maxHeight, availableHeight || 544);
        dropdown.style.maxHeight = maxHeight + 'px';
        dropdownRect = dropdown.getBoundingClientRect();
        var dropdownHeight = Math.min(dropdownRect.height || dropdown.offsetHeight || maxHeight, maxHeight);
        var spaceAbove = toggleRect.top - margin - gap;
        var spaceBelow = viewportHeight - toggleRect.bottom - margin - gap;
        var preferAbove = spaceAbove >= Math.min(dropdownHeight, 240) || spaceAbove >= spaceBelow;
        var top = preferAbove
            ? Math.max(margin, toggleRect.top - dropdownHeight - gap)
            : Math.min(viewportHeight - dropdownHeight - margin, toggleRect.bottom + gap);

        dropdown.style.left = Math.round(left) + 'px';
        dropdown.style.top = Math.round(top) + 'px';
    }

    bindDropdownListeners() {
        if (this.dropdownListenersBound) {
            return;
        }
        window.addEventListener('resize', this.boundPositionDropdown);
        window.addEventListener('scroll', this.boundPositionDropdown, true);
        this.dropdownListenersBound = true;
    }

    unbindDropdownListeners() {
        if (!this.dropdownListenersBound) {
            return;
        }
        window.removeEventListener('resize', this.boundPositionDropdown);
        window.removeEventListener('scroll', this.boundPositionDropdown, true);
        this.dropdownListenersBound = false;
    }

    setDropdownOpen(isOpen) {
        var dropdown = document.getElementById('theme-picker-dropdown');
        var toggle = document.getElementById('theme-picker-toggle');
        if (dropdown) {
            dropdown.style.display = isOpen ? 'block' : 'none';
            if (isOpen) {
                this.positionDropdown();
                this.bindDropdownListeners();
            } else {
                this.unbindDropdownListeners();
                dropdown.style.left = '';
                dropdown.style.top = '';
                dropdown.style.width = '';
                dropdown.style.maxWidth = '';
                dropdown.style.maxHeight = '';
            }
        }
        if (toggle) {
            toggle.setAttribute('aria-expanded', isOpen ? 'true' : 'false');
        }
    }
}

document.addEventListener('DOMContentLoaded', function () {
    window.themeManager = new ThemeManager();

    var dropdown = document.getElementById('theme-picker-dropdown');
    var toggle = document.getElementById('theme-picker-toggle');

    if (dropdown && toggle) {
        window.themeManager.buildPicker(dropdown);

        toggle.addEventListener('click', function (e) {
            e.stopPropagation();
            var isOpen = dropdown.style.display !== 'none';
            window.themeManager.setDropdownOpen(!isOpen);
        });

        document.addEventListener('click', function (e) {
            if (!e.target.closest('.theme-picker')) {
                window.themeManager.setDropdownOpen(false);
            }
        });
    }
});
