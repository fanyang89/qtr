import Cookies from 'js-cookie'

const DEFAULT_MAX_AGE = 60 * 60 * 24 * 7 // 7 days
const SECONDS_PER_DAY = 60 * 60 * 24

/**
 * Get a cookie value by name
 */
export function getCookie(name: string): string | undefined {
  if (typeof document === 'undefined') return undefined

  return Cookies.get(name)
}

/**
 * Set a cookie with name, value, and optional max age
 */
export function setCookie(
  name: string,
  value: string,
  maxAge: number = DEFAULT_MAX_AGE
): void {
  if (typeof document === 'undefined') return

  Cookies.set(name, value, { path: '/', expires: maxAge / SECONDS_PER_DAY })
}

/**
 * Remove a cookie by setting its max age to 0
 */
export function removeCookie(name: string): void {
  if (typeof document === 'undefined') return

  Cookies.remove(name, { path: '/' })
}
