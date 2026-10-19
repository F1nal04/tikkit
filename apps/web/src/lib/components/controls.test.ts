import { fireEvent, render, screen } from '@testing-library/svelte'
import { createRawSnippet } from 'svelte'
import { expect, test } from 'vitest'
import Alert from './Alert.svelte'
import Dialog from './Dialog.svelte'
import EmptyState from './EmptyState.svelte'
import ErrorView from './ErrorView.svelte'
import Field from './Field.svelte'
import StatusBadge from './StatusBadge.svelte'
import Table from './Table.svelte'

const text = (value: string) => createRawSnippet(() => ({ render: () => `<span>${value}</span>` }))
const row = (value: string) => createRawSnippet(() => ({ render: () => `<tr><td>${value}</td></tr>` }))

test('a field links its label, hint, and API error to the input', () => {
  render(Field, { props: { label: 'Username', name: 'username', hint: '3–64 characters', error: 'Taken' } })
  const input = screen.getByLabelText('Username')
  expect(input.getAttribute('aria-invalid')).toBe('true')
  expect(input.getAttribute('aria-describedby')).toBe('username-hint username-error')
  expect(document.getElementById('username-error')?.textContent).toBe('Taken')
})

test('a field without an error is valid and keeps what was typed when an error appears', async () => {
  const { rerender } = render(Field, { props: { label: 'Email', name: 'email' } })
  const input = screen.getByLabelText('Email') as HTMLInputElement
  expect(input.getAttribute('aria-invalid')).toBe('false')
  expect(input.getAttribute('aria-describedby')).toBeNull()
  await fireEvent.input(input, { target: { value: 'half-typed@' } })
  await rerender({ error: 'Not valid' })
  expect((screen.getByLabelText('Email') as HTMLInputElement).value).toBe('half-typed@')
  expect(input.getAttribute('aria-invalid')).toBe('true')
})

test('a field with options is a select', async () => {
  render(Field, {
    props: {
      label: 'Language',
      name: 'language',
      value: 'en',
      options: [
        { value: 'en', label: 'English' },
        { value: 'de', label: 'Deutsch' },
      ],
    },
  })
  const select = screen.getByRole('combobox', { name: 'Language' }) as HTMLSelectElement
  expect(select.value).toBe('en')
  await fireEvent.change(select, { target: { value: 'de' } })
  expect(select.value).toBe('de')
})

test('errors are announced, progress is polite', () => {
  render(Alert, { props: { children: text('Broken') } })
  expect(screen.getByRole('alert').textContent).toBe('Broken')
  render(Alert, { props: { kind: 'success', children: text('Saved') } })
  expect(screen.getByRole('status').textContent).toBe('Saved')
})

test('a table has a caption and column headers, or an empty state', () => {
  const head = createRawSnippet(() => ({ render: () => '<tr><th scope="col">Name</th></tr>' }))
  const { unmount } = render(Table, { props: { caption: 'Accounts', empty: 'None', isEmpty: false, head, children: row('alice') } })
  expect(screen.getByRole('table', { name: 'Accounts' })).toBeTruthy()
  expect(screen.getByRole('columnheader', { name: 'Name' })).toBeTruthy()
  // Wide tables scroll inside a focusable, labelled region.
  expect(screen.getByRole('region', { name: 'Accounts' }).getAttribute('tabindex')).toBe('0')
  unmount()
  render(Table, { props: { caption: 'Accounts', empty: 'No accounts.', isEmpty: true, head, children: row('x') } })
  expect(screen.queryByRole('table')).toBeNull()
  expect(screen.getByText('No accounts.')).toBeTruthy()
})

test('an empty state says what is missing', () => {
  render(EmptyState, { props: { title: 'Nothing here', children: text('Yet.') } })
  expect(screen.getByText('Nothing here')).toBeTruthy()
  expect(screen.getByText('Yet.')).toBeTruthy()
})

test('a status badge shows readable text and a stable identifier', () => {
  render(StatusBadge, { props: { status: 'awaiting_confirmation', label: 'Awaiting confirmation' } })
  const badge = screen.getByText('Awaiting confirmation')
  expect(badge.getAttribute('data-status')).toBe('awaiting_confirmation')
})

test('denied and missing pages read the same in both languages', () => {
  const { unmount } = render(ErrorView, { props: { status: 403, locale: 'en' } })
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Access denied')
  unmount()
  const again = render(ErrorView, { props: { status: 404, locale: 'de' } })
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Seite nicht gefunden')
  again.unmount()
  render(ErrorView, { props: { status: 500, locale: 'en' } })
  expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Something went wrong')
})

test('a dialog is labelled by its title and closes from its own controls', async () => {
  // jsdom has no modal dialog support; model the part of the platform the component relies on.
  const proto = HTMLDialogElement.prototype
  proto.showModal ??= function (this: HTMLDialogElement) {
    this.setAttribute('open', '')
  }
  proto.close ??= function (this: HTMLDialogElement) {
    this.removeAttribute('open')
    this.dispatchEvent(new Event('close'))
  }
  const actions = createRawSnippet(() => ({ render: () => '<button type="button">Cancel</button>' }))
  const { rerender } = render(Dialog, { props: { open: false, title: 'Deactivate alice?', children: text('Sure?'), actions } })
  const dialog = document.querySelector('dialog')!
  expect(dialog.hasAttribute('open')).toBe(false)
  await rerender({ open: true })
  expect(dialog.hasAttribute('open')).toBe(true)
  expect(screen.getByRole('dialog', { name: 'Deactivate alice?' })).toBe(dialog)
  expect(screen.getByRole('button', { name: 'Cancel' })).toBeTruthy()
  await rerender({ open: false })
  expect(dialog.hasAttribute('open')).toBe(false)
})
