// English and German messages for API error codes. The API's `message` is
// only an English fallback; the UI always translates by `code`. The typed
// records make tsc fail when the server adds a code without translations.
// (A full i18n layer arrives with the auth UI; keep this reusable.)
import type { ApiError, ApiErrorCode, FieldError, FieldErrorCode } from './client'

export type Locale = 'en' | 'de'

type Messages<K extends string> = Record<Locale, Record<K, string>>

export const errorMessages: Messages<ApiErrorCode> = {
  en: {
    invalid_request: 'The request could not be processed. Please reload the page and try again.',
    validation_failed: 'Please correct the highlighted fields.',
    unauthenticated: 'Please sign in to continue.',
    forbidden: 'You do not have permission to do this.',
    csrf_rejected: 'The request was blocked for security reasons. Please reload the page and try again.',
    not_found: 'This item does not exist or you do not have access to it.',
    method_not_allowed: 'This action is not supported.',
    conflict: 'This item was changed in the meantime. Please reload and try again.',
    invalid_credentials: 'The username or password is incorrect.',
    setup_completed: 'Setup has already been completed. Please sign in.',
    last_admin: 'The last active administrator cannot be demoted or deactivated.',
    team_membership_conflict: 'This change conflicts with the account’s team membership. Resolve the membership first.',
    team_lead_required: 'This account leads a team. Appoint a replacement team lead first.',
    team_has_open_tickets: 'This team still has open tickets. Transfer them before archiving the team.',
    payload_too_large: 'The submitted data is too large.',
    rate_limited: 'Too many attempts. Please wait a moment and try again.',
    internal_error: 'Something went wrong on our side. Please try again later.',
    service_unavailable: 'The service is temporarily unavailable. Please try again later.',
    network_error: 'The server could not be reached. Check your connection and try again.',
    unknown_error: 'An unexpected error occurred. Please try again.',
  },
  de: {
    invalid_request: 'Die Anfrage konnte nicht verarbeitet werden. Bitte laden Sie die Seite neu und versuchen Sie es erneut.',
    validation_failed: 'Bitte korrigieren Sie die markierten Felder.',
    unauthenticated: 'Bitte melden Sie sich an, um fortzufahren.',
    forbidden: 'Sie haben keine Berechtigung für diese Aktion.',
    csrf_rejected: 'Die Anfrage wurde aus Sicherheitsgründen blockiert. Bitte laden Sie die Seite neu und versuchen Sie es erneut.',
    not_found: 'Dieses Element existiert nicht oder Sie haben keinen Zugriff darauf.',
    method_not_allowed: 'Diese Aktion wird nicht unterstützt.',
    conflict: 'Dieses Element wurde zwischenzeitlich geändert. Bitte laden Sie neu und versuchen Sie es erneut.',
    invalid_credentials: 'Benutzername oder Passwort ist falsch.',
    setup_completed: 'Die Ersteinrichtung wurde bereits abgeschlossen. Bitte melden Sie sich an.',
    last_admin: 'Der letzte aktive Administrator kann nicht herabgestuft oder deaktiviert werden.',
    team_membership_conflict: 'Diese Änderung steht im Konflikt mit der Teammitgliedschaft des Kontos. Klären Sie zuerst die Mitgliedschaft.',
    team_lead_required: 'Dieses Konto leitet ein Team. Bestimmen Sie zuerst eine neue Teamleitung.',
    team_has_open_tickets: 'Dieses Team hat noch offene Tickets. Übertragen Sie diese, bevor Sie das Team archivieren.',
    payload_too_large: 'Die übermittelten Daten sind zu groß.',
    rate_limited: 'Zu viele Versuche. Bitte warten Sie einen Moment und versuchen Sie es erneut.',
    internal_error: 'Auf unserer Seite ist ein Fehler aufgetreten. Bitte versuchen Sie es später erneut.',
    service_unavailable: 'Der Dienst ist vorübergehend nicht verfügbar. Bitte versuchen Sie es später erneut.',
    network_error: 'Der Server ist nicht erreichbar. Bitte prüfen Sie Ihre Verbindung und versuchen Sie es erneut.',
    unknown_error: 'Ein unerwarteter Fehler ist aufgetreten. Bitte versuchen Sie es erneut.',
  },
}

/** Field messages; `{name}` placeholders are filled from `params`. */
export const fieldErrorMessages: Messages<FieldErrorCode> = {
  en: {
    required: 'This field is required.',
    too_short: 'Must be at least {min} characters.',
    too_long: 'Must be at most {max} characters.',
    out_of_range: 'Must be between {min} and {max}.',
    invalid_format: 'The format is not valid.',
    invalid_value: 'This value is not allowed.',
    invalid_type: 'This value has the wrong type.',
    unknown_field: 'This field is not supported.',
    already_taken: 'This value is already in use.',
  },
  de: {
    required: 'Dieses Feld ist erforderlich.',
    too_short: 'Muss mindestens {min} Zeichen lang sein.',
    too_long: 'Darf höchstens {max} Zeichen lang sein.',
    out_of_range: 'Muss zwischen {min} und {max} liegen.',
    invalid_format: 'Das Format ist ungültig.',
    invalid_value: 'Dieser Wert ist nicht zulässig.',
    invalid_type: 'Dieser Wert hat den falschen Typ.',
    unknown_field: 'Dieses Feld wird nicht unterstützt.',
    already_taken: 'Dieser Wert wird bereits verwendet.',
  },
}

/** Label for showing the request id to users (for support requests). */
export const requestIdLabel: Record<Locale, string> = {
  en: 'Request ID',
  de: 'Anfrage-ID',
}

function interpolate(template: string, params: Record<string, unknown> | undefined): string {
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params?.[name]
    return value === undefined || value === null ? match : String(value)
  })
}

export function errorMessage(error: Pick<ApiError, 'code'>, locale: Locale): string {
  return errorMessages[locale][error.code]
}

export function fieldErrorMessage(field: FieldError, locale: Locale): string {
  return interpolate(fieldErrorMessages[locale][field.code], field.params as Record<string, unknown>)
}
