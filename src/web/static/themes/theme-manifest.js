/* Symlinkarr - Built-in theme manifest */

(function () {
    function clamp01(value) {
        return Math.max(0, Math.min(1, value));
    }

    function normalizeHex(hex) {
        var value = (hex || '').replace('#', '').trim();
        if (value.length === 3) {
            return '#' + value.split('').map(function (part) {
                return part + part;
            }).join('');
        }
        return '#' + value.padStart(6, '0').slice(0, 6);
    }

    function hexToRgb(hex) {
        var normalized = normalizeHex(hex).slice(1);
        return {
            r: parseInt(normalized.slice(0, 2), 16),
            g: parseInt(normalized.slice(2, 4), 16),
            b: parseInt(normalized.slice(4, 6), 16),
        };
    }

    function rgbToHex(rgb) {
        return '#' + [rgb.r, rgb.g, rgb.b].map(function (value) {
            return Math.max(0, Math.min(255, Math.round(value))).toString(16).padStart(2, '0');
        }).join('');
    }

    function rgbString(hex) {
        var rgb = hexToRgb(hex);
        return rgb.r + ', ' + rgb.g + ', ' + rgb.b;
    }

    function mix(hexA, hexB, amount) {
        var a = hexToRgb(hexA);
        var b = hexToRgb(hexB);
        var ratio = clamp01(amount);
        return rgbToHex({
            r: a.r + ((b.r - a.r) * ratio),
            g: a.g + ((b.g - a.g) * ratio),
            b: a.b + ((b.b - a.b) * ratio),
        });
    }

    function rgba(hex, alpha) {
        var rgb = hexToRgb(hex);
        return 'rgba(' + rgb.r + ', ' + rgb.g + ', ' + rgb.b + ', ' + clamp01(alpha) + ')';
    }

    function contrastText(hex) {
        var rgb = hexToRgb(hex);
        var luminance = (0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b) / 255;
        return luminance > 0.58 ? '#0f172a' : '#ffffff';
    }

    function buildTheme(theme) {
        if (!theme.palette) {
            return theme;
        }

        var palette = theme.palette;
        var isDark = theme.mode === 'dark';
        var base = normalizeHex(palette.base);
        var text = normalizeHex(palette.text);
        var primary = normalizeHex(palette.primary);
        var tool = normalizeHex(palette.tool || palette.info || palette.primary);
        var secondary = normalizeHex(palette.secondary || mix(text, base, isDark ? 0.55 : 0.35));
        var success = normalizeHex(palette.success || (isDark ? '#4ade80' : '#16a34a'));
        var error = normalizeHex(palette.error || (isDark ? '#fb7185' : '#dc2626'));
        var info = normalizeHex(palette.info || palette.tool || palette.primary);
        var warning = normalizeHex(palette.warning || (isDark ? '#fbbf24' : '#d97706'));

        var bgSecondary = normalizeHex(palette.bgSecondary || mix(base, isDark ? '#ffffff' : '#000000', isDark ? 0.035 : 0.02));
        var bgTertiary = normalizeHex(palette.bgTertiary || mix(base, isDark ? '#ffffff' : '#000000', isDark ? 0.07 : 0.045));
        var cardBg = normalizeHex(palette.cardBg || mix(base, isDark ? '#ffffff' : '#000000', isDark ? 0.09 : 0.035));
        var cardBgElevated = normalizeHex(palette.cardBgElevated || mix(base, isDark ? '#ffffff' : '#000000', isDark ? 0.12 : 0.015));
        var borderColor = normalizeHex(palette.borderColor || mix(base, text, isDark ? 0.14 : 0.18));
        var borderStrong = normalizeHex(palette.borderStrong || mix(base, text, isDark ? 0.22 : 0.28));

        var shellBg = normalizeHex(palette.shellBg || (isDark
            ? mix(base, primary, 0.16)
            : mix(primary, '#0f172a', 0.78)));
        var shellHoverBg = normalizeHex(palette.shellHoverBg || mix(shellBg, isDark ? '#ffffff' : '#0f172a', isDark ? 0.08 : 0.08));
        var shellDropdownBg = normalizeHex(palette.shellDropdownBg || mix(shellBg, isDark ? '#ffffff' : '#0f172a', isDark ? 0.03 : 0.04));
        var shellTextPrimary = normalizeHex(palette.shellTextPrimary || contrastText(shellBg));
        var shellTextSecondary = rgba(shellTextPrimary, 0.82);
        var shellTextFaint = rgba(shellTextPrimary, 0.62);

        var accentPrimary = primary;
        var accentSecondary = tool;
        var brandPrimary = normalizeHex(palette.brandPrimary || primary);
        var brandPrimaryRgb = rgbString(brandPrimary);

        var vars = {
            '--bg-primary': base,
            '--bg-secondary': bgSecondary,
            '--bg-tertiary': bgTertiary,
            '--card-bg': cardBg,
            '--card-bg-elevated': cardBgElevated,
            '--brand-primary': brandPrimary,
            '--brand-primary-rgb': brandPrimaryRgb,
            '--accent-primary': accentPrimary,
            '--accent-secondary': accentSecondary,
            '--text-primary': text,
            '--text-secondary': normalizeHex(palette.textSecondary || mix(text, base, isDark ? 0.18 : 0.28)),
            '--text-faint': normalizeHex(palette.textFaint || mix(text, base, isDark ? 0.38 : 0.48)),
            '--success': success,
            '--warning': warning,
            '--error': error,
            '--info': info,
            '--border-color': borderColor,
            '--border-strong': borderStrong,
            '--shadow-subtle': isDark ? '0 12px 32px rgba(0, 0, 0, 0.32)' : '0 20px 48px rgba(15, 23, 42, 0.08)',
            '--surface-muted': isDark ? 'rgba(255, 255, 255, 0.04)' : rgba(text, 0.04),
            '--shell-bg': shellBg,
            '--shell-hover-bg': shellHoverBg,
            '--shell-panel-bg': palette.shellPanelBg || (isDark ? rgba(primary, 0.10) : 'rgba(255, 255, 255, 0.08)'),
            '--shell-panel-border': palette.shellPanelBorder || (isDark ? rgba(primary, 0.18) : 'rgba(255, 255, 255, 0.12)'),
            '--shell-dropdown-bg': shellDropdownBg,
            '--shell-border-color': palette.shellBorderColor || (isDark ? rgba(primary, 0.16) : 'rgba(255, 255, 255, 0.10)'),
            '--shell-border-strong': palette.shellBorderStrong || (isDark ? rgba(primary, 0.26) : 'rgba(255, 255, 255, 0.18)'),
            '--shell-text-primary': shellTextPrimary,
            '--shell-text-secondary': shellTextSecondary,
            '--shell-text-faint': shellTextFaint,
            '--logo-mark-bg-start': palette.logoMarkStart || rgba(brandPrimary, isDark ? 0.22 : 0.18),
            '--logo-mark-bg-end': palette.logoMarkEnd || rgba(brandPrimary, isDark ? 0.08 : 0.06),
            '--logo-mark-border': palette.logoMarkBorder || rgba(brandPrimary, isDark ? 0.32 : 0.24),
            '--nav-icon-bg': rgba(shellTextPrimary, isDark ? 0.04 : 0.06),
            '--nav-icon-border': rgba(shellTextPrimary, isDark ? 0.08 : 0.10),
            '--nav-icon-active-bg': rgba(brandPrimary, isDark ? 0.12 : 0.14),
            '--nav-icon-active-border': rgba(brandPrimary, isDark ? 0.28 : 0.22),
            '--badge-success-bg': rgba(success, isDark ? 0.16 : 0.12),
            '--badge-success-border': rgba(success, isDark ? 0.30 : 0.24),
            '--badge-warning-bg': rgba(warning, isDark ? 0.16 : 0.12),
            '--badge-warning-border': rgba(warning, isDark ? 0.30 : 0.24),
            '--badge-danger-bg': rgba(error, isDark ? 0.16 : 0.12),
            '--badge-danger-border': rgba(error, isDark ? 0.30 : 0.24),
            '--badge-info-bg': rgba(info, isDark ? 0.16 : 0.12),
            '--badge-info-border': rgba(info, isDark ? 0.30 : 0.24),
            '--badge-secondary-bg': rgba(secondary, isDark ? 0.16 : 0.10),
            '--badge-secondary-border': rgba(secondary, isDark ? 0.28 : 0.20),
            '--btn-primary-hover': normalizeHex(palette.btnPrimaryHover || mix(primary, isDark ? '#ffffff' : '#000000', 0.12)),
            '--btn-secondary-hover': normalizeHex(palette.btnSecondaryHover || mix(cardBg, text, isDark ? 0.08 : 0.04)),
            '--btn-danger-hover': normalizeHex(palette.btnDangerHover || mix(error, isDark ? '#ffffff' : '#000000', 0.12)),
            '--text-on-accent': palette.textOnAccent || contrastText(primary),
            '--focus-ring': rgba(primary, isDark ? 0.22 : 0.18),
            '--alert-success-bg': rgba(success, isDark ? 0.20 : 0.14),
            '--alert-error-bg': rgba(error, isDark ? 0.20 : 0.14),
            '--alert-warning-bg': rgba(warning, isDark ? 0.20 : 0.14),
            '--dropdown-shadow': isDark ? '0 20px 48px rgba(0, 0, 0, 0.42)' : '0 20px 48px rgba(15, 23, 42, 0.14)',
            '--theme-swatch-border': isDark ? 'rgba(255, 255, 255, 0.12)' : 'rgba(23, 32, 43, 0.10)',
            '--toggle-track-bg': isDark ? normalizeHex(mix(base, '#ffffff', 0.12)) : normalizeHex(mix(base, '#0f172a', 0.12)),
            '--toggle-track-border': isDark ? normalizeHex(mix(base, '#ffffff', 0.22)) : normalizeHex(mix(base, '#0f172a', 0.18)),
            '--toggle-thumb-bg': isDark ? '#ffffff' : '#ffffff',
            '--toggle-track-active-bg': rgba(primary, isDark ? 0.18 : 0.14),
            '--toggle-track-active-border': rgba(primary, isDark ? 0.28 : 0.22),
        };

        return {
            id: theme.id,
            name: theme.name,
            group: theme.group,
            mode: theme.mode,
            swatches: theme.swatches || palette.gradientColors || [base, primary, tool],
            vars: vars,
        };
    }

    var themeDefinitions = [
        {
            id: 'auto',
            name: 'Auto',
            group: 'Core',
            swatches: ['#f4f7fb', '#35c5f4', '#111827'],
        },
        {
            id: 'symlinkarr-dark',
            name: 'Symlinkarr Dark',
            group: 'Core',
            mode: 'dark',
            palette: {
                text: '#dbe7f5',
                base: '#111827',
                primary: '#35c5f4',
                tool: '#7dd3fc',
                secondary: '#5b6f8d',
                success: '#4ade80',
                error: '#fb7185',
                info: '#60a5fa',
                warning: '#fbbf24',
            },
        },
        {
            id: 'symlinkarr-light',
            name: 'Symlinkarr Light',
            group: 'Core',
            mode: 'light',
            palette: {
                text: '#1f2937',
                base: '#f4f7fb',
                primary: '#35c5f4',
                tool: '#1184d1',
                secondary: '#64748b',
                success: '#16a34a',
                error: '#dc2626',
                info: '#0284c7',
                warning: '#d97706',
            },
        },
        {
            id: 'matrix',
            name: 'Matrix',
            group: 'Core',
            mode: 'dark',
            palette: {
                text: '#d6ffe0',
                base: '#030706',
                primary: '#6dff8b',
                tool: '#2dd4bf',
                secondary: '#2a5136',
                success: '#86efac',
                error: '#fb7185',
                info: '#5eead4',
                warning: '#fef08a',
            },
        },
        {
            id: 'catppuccin-latte',
            name: 'Catppuccin Latte',
            group: 'Catppuccin',
            mode: 'light',
            palette: {
                text: '#4c4f69',
                base: '#eff1f5',
                primary: '#8839ef',
                tool: '#1e66f5',
                secondary: '#5c5f77',
                success: '#40a02b',
                error: '#d20f39',
                info: '#209fb5',
                warning: '#df8e1d',
                gradientColors: ['#dc8a78', '#8839ef', '#1e66f5'],
            },
        },
        {
            id: 'catppuccin-frappe',
            name: 'Catppuccin Frappé',
            group: 'Catppuccin',
            mode: 'dark',
            palette: {
                text: '#c6d0f5',
                base: '#303446',
                primary: '#ea999c',
                tool: '#81c8be',
                secondary: '#b5bfe2',
                success: '#a6d189',
                error: '#e78284',
                info: '#99d1db',
                warning: '#e5c890',
                gradientColors: ['#ea999c', '#81c8be', '#99d1db'],
            },
        },
        {
            id: 'catppuccin-macchiato',
            name: 'Catppuccin Macchiato',
            group: 'Catppuccin',
            mode: 'dark',
            palette: {
                text: '#cad3f5',
                base: '#24273a',
                primary: '#f5a97f',
                tool: '#91d7e3',
                secondary: '#b8c0e0',
                success: '#a6da95',
                error: '#ed8796',
                info: '#8aadf4',
                warning: '#eed49f',
                gradientColors: ['#f5a97f', '#91d7e3', '#8aadf4'],
            },
        },
        {
            id: 'catppuccin-mocha',
            name: 'Catppuccin Mocha',
            group: 'Catppuccin',
            mode: 'dark',
            palette: {
                text: '#cdd6f4',
                base: '#1e1e2e',
                primary: '#cba6f7',
                tool: '#89b4fa',
                secondary: '#bac2de',
                success: '#a6e3a1',
                error: '#f38ba8',
                info: '#74c7ec',
                warning: '#f9e2af',
                gradientColors: ['#f5e0dc', '#cba6f7', '#89b4fa'],
            },
        },
        {
            id: 'tokyo-night',
            name: 'Tokyo Night',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#c0caf5',
                base: '#1a1b26',
                primary: '#bb9af7',
                tool: '#7dcfff',
                secondary: '#565f89',
                success: '#7af778',
                error: '#f7768e',
                info: '#2ac3de',
                warning: '#e0af68',
            },
        },
        {
            id: 'kanagawa',
            name: 'Kanagawa',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#dcd7ba',
                base: '#1f2335',
                primary: '#c792ea',
                tool: '#7aa2f7',
                secondary: '#6c7a89',
                success: '#9ece6a',
                error: '#f7768e',
                info: '#2ac3de',
                warning: '#e0af68',
            },
        },
        {
            id: 'nord-frost',
            name: 'Nord Frost',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#eceff4',
                base: '#2e3440',
                primary: '#88c0d0',
                tool: '#8fbcbb',
                secondary: '#4c566a',
                success: '#a3be8c',
                error: '#bf616a',
                info: '#5e81ac',
                warning: '#ebcb8b',
            },
        },
        {
            id: 'rose-pine',
            name: 'Rosé Pine',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#e0def4',
                base: '#191724',
                primary: '#ebbcba',
                tool: '#9ccfd8',
                secondary: '#6e6a86',
                success: '#31748f',
                error: '#eb6f92',
                info: '#c4a7e7',
                warning: '#f6c177',
            },
        },
        {
            id: 'rose-pine-dawn',
            name: 'Rosé Pine Dawn',
            group: 'Nanocoder Picks',
            mode: 'light',
            palette: {
                text: '#575279',
                base: '#faf4ed',
                primary: '#907aa9',
                tool: '#56949f',
                secondary: '#9893a5',
                success: '#618a3d',
                error: '#b4637a',
                info: '#286983',
                warning: '#ea9d34',
            },
        },
        {
            id: 'gruvbox-dark',
            name: 'Gruvbox Dark',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#ebdbb2',
                base: '#282828',
                primary: '#fe8019',
                tool: '#83a598',
                secondary: '#928374',
                success: '#b8bb26',
                error: '#fb4934',
                info: '#8ec07c',
                warning: '#fabd2f',
            },
        },
        {
            id: 'gruvbox-light',
            name: 'Gruvbox Light',
            group: 'Nanocoder Picks',
            mode: 'light',
            palette: {
                text: '#3c3836',
                base: '#fbf1c7',
                primary: '#af3a03',
                tool: '#076678',
                secondary: '#928374',
                success: '#79740e',
                error: '#9d0006',
                info: '#427b58',
                warning: '#b57614',
            },
        },
        {
            id: 'one-dark',
            name: 'One Dark',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#abb2bf',
                base: '#282c34',
                primary: '#61afef',
                tool: '#56b6c2',
                secondary: '#5c6370',
                success: '#98c379',
                error: '#e06c75',
                info: '#c678dd',
                warning: '#e5c07b',
            },
        },
        {
            id: 'one-light',
            name: 'One Light',
            group: 'Nanocoder Picks',
            mode: 'light',
            palette: {
                text: '#383a42',
                base: '#fafafa',
                primary: '#4078f2',
                tool: '#0184bc',
                secondary: '#a0a1a7',
                success: '#50a14f',
                error: '#e45649',
                info: '#a626a4',
                warning: '#c18401',
            },
        },
        {
            id: 'dracula',
            name: 'Dracula',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#f8f8f2',
                base: '#282a36',
                primary: '#ff79c6',
                tool: '#8be9fd',
                secondary: '#bd93f9',
                success: '#50fa7b',
                error: '#ff5555',
                info: '#f1fa8c',
                warning: '#ffb86c',
            },
        },
        {
            id: 'night-owl',
            name: 'Night Owl',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#d6deeb',
                base: '#011627',
                primary: '#82aaff',
                tool: '#7fdbca',
                secondary: '#637777',
                success: '#addb67',
                error: '#ef5350',
                info: '#c792ea',
                warning: '#f78c6c',
            },
        },
        {
            id: 'midnight-amethyst',
            name: 'Midnight Amethyst',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#e6e6fa',
                base: '#0f0a1a',
                primary: '#9966cc',
                tool: '#ba55d3',
                secondary: '#6a5acd',
                success: '#9370db',
                error: '#dc143c',
                info: '#c0c0c0',
                warning: '#dda0dd',
            },
        },
        {
            id: 'cherry-blossom',
            name: 'Cherry Blossom',
            group: 'Nanocoder Picks',
            mode: 'light',
            palette: {
                text: '#5a5a5a',
                base: '#fef7f0',
                primary: '#ffb6c1',
                tool: '#20b2aa',
                secondary: '#bc8f8f',
                success: '#3cb371',
                error: '#cd5c5c',
                info: '#4682b4',
                warning: '#f4a460',
            },
        },
        {
            id: 'aurora-borealis',
            name: 'Aurora Borealis',
            group: 'Nanocoder Picks',
            mode: 'dark',
            palette: {
                text: '#e8f4f8',
                base: '#0a0f14',
                primary: '#00ff9f',
                tool: '#00d4ff',
                secondary: '#7b68ee',
                success: '#39ff14',
                error: '#ff006e',
                info: '#00ffff',
                warning: '#ffd700',
                gradientColors: ['#00ff9f', '#00d4ff', '#7b68ee'],
            },
        },
    ];

    var themes = themeDefinitions.map(buildTheme);
    var themeIndex = {};
    themes.forEach(function (theme) {
        themeIndex[theme.id] = theme;
    });

    var defaults = {
        dark: 'symlinkarr-dark',
        light: 'symlinkarr-light',
    };

    var aliases = {
        dark: defaults.dark,
        light: defaults.light,
        'sonarr-dark': defaults.dark,
        'sonarr-light': defaults.light,
        'radarr-dark': defaults.dark,
        'radarr-light': defaults.light,
        'prowlarr-dark': defaults.dark,
        'prowlarr-light': defaults.light,
        'lidarr-dark': defaults.dark,
        'lidarr-light': defaults.light,
        'readarr-dark': defaults.dark,
        'readarr-light': defaults.light,
    };

    function getTheme(themeId) {
        return themeIndex[themeId] || null;
    }

    function resolveSelection(themeId, prefersDark) {
        var selectionId = aliases[themeId] || themeId || 'auto';
        var actualId = selectionId === 'auto'
            ? (prefersDark ? defaults.dark : defaults.light)
            : selectionId;
        var selection = getTheme(selectionId) || getTheme('auto');
        var actual = getTheme(actualId) || getTheme(defaults.dark);

        return {
            selectionId: selection ? selection.id : 'auto',
            selection: selection,
            actual: actual,
        };
    }

    function buildThemeCss(theme) {
        if (!theme || !theme.vars) {
            return '';
        }

        var lines = [':root {', '  color-scheme: ' + theme.mode + ';'];
        Object.keys(theme.vars).forEach(function (key) {
            lines.push('  ' + key + ': ' + theme.vars[key] + ';');
        });
        lines.push('}');
        return lines.join('\n');
    }

    window.SYMLINKARR_THEME_MANIFEST = {
        themes: themes,
        aliases: aliases,
        defaults: defaults,
        getTheme: getTheme,
        resolveSelection: resolveSelection,
        buildThemeCss: buildThemeCss,
    };
}());
