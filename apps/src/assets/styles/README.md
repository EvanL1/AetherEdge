# Style System

This project now uses a layered style system. Keep new styles inside the correct layer instead of patching Element Plus ad hoc.

## Layers

1. `tokens.css`
   Defines project design tokens such as color, spacing, radius, typography, and control size.

2. `element/css-vars.css`
   Maps project tokens to Element Plus CSS variables. Prefer changing this file when the goal is to restyle Element Plus globally.

3. `base.css`
   Contains global document-level rules only: sizing, body defaults, typography resets, and shared utility visuals like scrollbars.

4. `element/*.scss`
   Contains structural overrides for Element Plus components when CSS variables are not enough.

## Rules

- Prefer `:root` variables over hard-coded values.
- Prefer `--el-*` variables over deep selectors when Element Plus exposes them.
- Avoid `!important` unless you are correcting a third-party inline or teleport style.
- Avoid default fixed widths on base controls. Use container sizing or local utility classes.
- Keep overlay, popper, and dialog positioning close to library defaults unless there is a verified layout bug.
- Page-specific patches belong in the page component, not in global element skin files.

## Import order

The only global entry is `src/assets/main.css` and it must stay ordered as:

1. project tokens
2. Element Plus CSS variables
3. base styles
4. fonts
5. Element Plus structural overrides
6. third-party feature styles
