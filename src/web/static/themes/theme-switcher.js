/* Symlinkarr Web UI - Theme Switcher */
/* Allows users to switch between different visual themes */

/**
 * Theme manager for Symlinkarr web interface
 */
class ThemeManager {
    constructor() {
        this.themes = {
            'dark': 'dark-theme.css',
            'light': 'light-theme.css',
            'accessibility': 'accessibility-theme.css',
            'colorblind': 'colorblind-theme.css',
            'compact': 'compact-theme.css'
        };

        this.currentTheme = this.loadTheme() || 'dark';
        this.themeLink = null;

        this.init();
    }

    init() {
        // Create theme link element if it doesn't exist
        this.themeLink = document.createElement('link');
        this.themeLink.rel = 'stylesheet';
        this.themeLink.id = 'theme-stylesheet';

        // Remove any existing theme stylesheet
        const existingTheme = document.getElementById('theme-stylesheet');
        if (existingTheme) {
            existingTheme.remove();
        }

        // Apply current theme
        this.applyTheme(this.currentTheme);

        // Add to document head
        document.head.appendChild(this.themeLink);
    }

    applyTheme(themeName) {
        if (!this.themes[themeName]) {
            console.warn(`Theme "${themeName}" not found. Falling back to dark.`);
            themeName = 'dark';
        }

        this.currentTheme = themeName;
        this.themeLink.href = `/src/web/static/themes/${this.themes[themeName]}`;

        // Save preference
        this.saveTheme(themeName);

        // Trigger theme change event
        document.dispatchEvent(new CustomEvent('themechanged', {
            detail: { theme: themeName }
        }));
    }

    getCurrentTheme() {
        return this.currentTheme;
    }

    loadTheme() {
        try {
            return localStorage.getItem('symlinkarr-theme');
        } catch (e) {
            console.warn('Unable to load theme from localStorage:', e);
            return null;
        }
    }

    saveTheme(themeName) {
        try {
            localStorage.setItem('symlinkarr-theme', themeName);
        } catch (e) {
            console.warn('Unable to save theme to localStorage:', e);
        }
    }

    // Theme cycling for easy switching
    cycleTheme() {
        const themeList = Object.keys(this.themes);
        const currentIndex = themeList.indexOf(this.currentTheme);
        const nextIndex = (currentIndex + 1) % themeList.length;
        this.applyTheme(themeList[nextIndex]);
    }

    // Get available themes
    getAvailableThemes() {
        return Object.keys(this.themes);
    }
}

// Initialize theme manager when DOM is loaded
document.addEventListener('DOMContentLoaded', () => {
    window.themeManager = new ThemeManager();

    // Add theme switcher to UI if desired
    // This would typically be added to the header or settings panel
});

/* Example usage in HTML:
// Add to header or settings:
// <button onclick="themeManager.cycleTheme()" title="Switch Theme">🎨</button>
//
// Or for direct selection:
// <select onchange="themeManager.applyTheme(this.value)">
//   <option value="dark">Dark</option>
//   <option value="light">Light</option>
//   <option value="accessibility">Accessibility</option>
//   <option value="colorblind">Colorblind</option>
//   <option value="compact">Compact</option>
// </select>
*/