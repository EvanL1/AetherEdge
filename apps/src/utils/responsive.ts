// Responsive layout utilities

const DESIGN_WIDTH = 1920
const BASE_FONT_SIZE = 100 // 100px = 1rem

function getResponsiveScale(): number {
  return window.innerWidth / DESIGN_WIDTH
}

/**
 * Set the root font size for rem-based responsive layouts.
 */
export function setRem(): void {
  const html = document.documentElement
  const fontSize = BASE_FONT_SIZE * getResponsiveScale()
  html.style.fontSize = `${fontSize}px`
}

/**
 * Initialize responsive behavior.
 */
export function initResponsive(): void {
  setRem()

  window.addEventListener('resize', function () {
    clearTimeout((window as any).remResizeTimer)
    ;(window as any).remResizeTimer = setTimeout(setRem, 100)
  })
}

/**
 * Convert a design-time px value into a px value under the current viewport scale.
 */
export function pxToResponsive(designPx: number): number {
  return designPx * getResponsiveScale()
}

/**
 * Convert a design-time px value into rem.
 */
export function pxToRem(designPx: number): string {
  const currentBaseFontSize = BASE_FONT_SIZE * getResponsiveScale()
  const remValue = designPx / currentBaseFontSize
  return `${remValue}rem`
}

/**
 * Get the current viewport scale relative to the design size.
 */
export function getCurrentScale(): number {
  return getResponsiveScale()
}

/**
 * Get the current root font size in px.
 */
export function getCurrentFontSize(): number {
  const html = document.documentElement
  const fontSize = parseFloat(getComputedStyle(html).fontSize)
  return fontSize || BASE_FONT_SIZE
}
