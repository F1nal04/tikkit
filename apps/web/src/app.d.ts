import type { Locale } from '#lib/i18n.ts'

declare global {
  namespace App {
    interface Locals {
      locale: Locale
    }
  }
}

export {}
