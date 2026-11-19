# Requester guide

For everyone who submits and follows tickets. Every role (User, Operator,
Specialist, Team Lead, Admin) uses the portal the same way for their own
tickets. The portal is available in English and German; this guide is in English.

What your organization has switched on varies. Where a feature depends on an
Administrator's setting, this guide says so. Ask your Admin if something
described here does not appear.

## Sign in and your account

- You get an account from an Admin **invitation link**. There is no public
  signup. Open the link, choose a username (3–64 characters: letters, digits,
  dot, underscore, hyphen) and a password (at least 12 characters), and
  choose **Create account**. The role comes from the invitation; you cannot
  change it. The link works once, and only a successful account creation uses it.
- You sign in with your **username** and password. The email on your account
  is only a contact address for notifications. Several accounts can share one
  email, and they never share permissions or tickets. A person who has both an
  operational account and an Admin account signs in separately to each.
- **Forgot password?** Enter your username. If the account exists and your
  organization has set up email, a reset link goes to the email stored on that
  account. It is valid for one hour and works once. Without email setup, ask
  an Admin.
- **Language:** the language selector switches between English and Deutsch,
  also before you sign in. Your choice is saved on your account, with English
  as the fallback. Switching never changes a ticket, and unsent text stays in
  the form.
  Tickets, replies and any AI suggestion stay in the language they were
  written in; they are not translated when you switch.
- **Profile** lets you change your email, language and password, and set
  notification preferences.

## Create a ticket

Choose **New ticket**. Every category uses the same form. You submit only for
yourself; nobody can create a ticket on your behalf, and staff cannot change who
a ticket belongs to.

1. **Category.** Pick the one that fits. It decides which extra questions appear
   (for example, choosing a software category can ask for the program, with a
   list and an **Other (type your own answer)** option). The category never
   chooses a support team.
2. **Subject** and **Description.** Say what happened, what you expected, and any
   error message.
3. **Priority.** You choose the starting priority from the levels your Admin
   defined. Staff may raise or lower it later; you see the **Current priority**
   and your **Original priority**.
4. **Screenshots (optional).** PNG, JPEG or WebP images, up to 10 MiB each and 10
   per ticket. Animated images, other file types and documents are not accepted.
   Metadata such as location data is stripped when the image is stored.
5. Choose **Submit request**.

Drafts: the form saves a draft about a second after you stop typing, so you can
leave and **Resume** it later under **Saved drafts**. A submitted ticket is
frozen: the original details and screenshots can no longer be edited or deleted.

You see a receipt with the reference number. It says where the request goes next.

## AI assistance (only if your organization uses it)

AI assistance is optional and off by default. If your Admin enabled it, the form
shows an **AI assistance** notice. Nothing about AI appears when it is off.

When it applies to your category:

- **What is sent.** After you submit, your subject and description, the answers
  to the extra questions (such as the program), and any screenshots you attached
  are sent to the external AI provider your organization configured. Screenshots
  go as images. Your name and account, other tickets, and internal notes are not
  sent. Do not include passwords or keys in text or screenshots. Known secret
  patterns in text are removed before sending, but screenshots are not inspected.
- **It comes first.** For enabled categories, the AI suggestion happens before
  your request reaches the support team. You cannot skip it unless your
  organization allows skipping, in which case the form shows **Skip the AI
  suggestion and send this request straight to the support team**. Some
  categories are set to go straight to the support team without AI; the form says
  so, and nothing is sent to the provider.
- **One suggestion.** You get one proposed solution, shown as "AI-generated
  suggestion" in the language you used. It may be wrong. There is no chat, no
  follow-up questions and no second suggestion. Refreshing the page, switching
  language, or adding replies later never creates another one.
- **You decide.** Choose **Solved** or **Still need help**.
  - **Solved** records that the AI suggestion solved your problem. The record
    is final and read-only: it is not sent to support and **cannot be reopened
    or escalated by anyone**. If the problem comes back, create a new ticket.
  - **Still need help** sends this same request, unchanged and with the same
    reference, to the support team.
  - Leaving the page, doing nothing, or viewing the suggestion decides nothing.
- **If AI fails.** If no suggestion can be prepared, or it takes too long, your
  request goes to the support team automatically, exactly once. You do not need
  to submit it again. Waiting usually takes under a minute but can take several
  minutes; you can leave the page and return from **My tickets**.
- **Your history.** An AI-solved ticket stays in **My tickets**, read-only, with
  the suggestion and the time you confirmed it. Your organization's Admins can
  see it and count it in statistics; support staff do not see it.

## Where your ticket goes

A request that needs a person enters the **shared general queue**, which every
Operator can see. An Operator then sends it to a team; nothing is assigned
automatically by category, priority or AI outcome. A member of that team claims
it, or the team's Team Lead assigns it.

Your ticket page shows **Handling team** and **Assigned to**: "General queue"
and "Unassigned" while it waits, then the team name and the staff member's
name. After it moves to a team, only members of that team and Admins can see it;
you always keep access to your own ticket. Staff cannot handle a ticket
**they submitted themselves**, whatever their role; another staff member does.

### Statuses

| Status | Meaning |
| --- | --- |
| AI suggestion in progress | The AI is preparing its one suggestion (only if AI applies). |
| Awaiting your decision on the AI suggestion | Choose **Solved** or **Still need help**. |
| Queued | Waiting for support. |
| In progress | Support is working on it. |
| Waiting for requester information | Support asked you something. Reply to continue. |
| Awaiting requester confirmation | Support proposed a resolution. See below. |
| Closed | Finished. |
| Cancelled | You cancelled it. |
| Solved by AI | You confirmed the AI suggestion. Final. |

## Reply and add information

Open the ticket from **My tickets**. You can add **public replies**, with
screenshots, while the ticket is open (queued, in progress, waiting for
information, or awaiting confirmation). Replies are permanent: you cannot edit
or delete earlier content, and later replies do not trigger new AI suggestions.
A reply while the status is Waiting for requester information moves the ticket
back to In progress. You never see internal notes; support staff keep those private.

Use **Refresh** to see the newest state. Staff pages update automatically; yours
updates when you refresh (except while the AI step is waiting).

## When support proposes a resolution

Support cannot close a ticket on their own. A proposal moves it to **Awaiting
requester confirmation** with a summary and an **Automatic closure deadline**.
You choose:

- **Confirm resolved:** the ticket closes with your confirmation.
- **Still not solved:** the same ticket returns to active support with its team
  and history. This is not "reopening" and does not depend on any setting.

If you do nothing, the ticket closes automatically at the deadline and is
recorded as closed automatically, not as confirmed by you. Silence is never
acceptance, but the deadline is real; the default is 168 hours (7 days) from the
proposal and your Admin can change it for later proposals. A plain reply does not
confirm or reject. Only tickets awaiting confirmation have this deadline; queued
tickets, tickets waiting for your information, and pending AI steps do not.

## Reopen a closed ticket (only if allowed)

After a ticket closed, **Reopen ticket** appears only when your Admin allows
reopening operator-resolved tickets. It is off by default, so usually you open a
new ticket. When it is allowed, reopening returns the same ticket to In
progress. Tickets solved by AI, cancelled tickets, and tickets whose content was
removed under the retention policy cannot be reopened.

## Cancel a ticket

**Cancel ticket** is available according to your organization's policy:

- **Anytime while open** (the default),
- **Only before work starts** (until support first sets it In progress), or
- **Disabled**.

You must give a reason (up to 1,000 characters), then choose **Confirm
cancellation**; **Keep ticket** backs out. The ticket stays in your history with
the reason. Cancelling ends pending AI work and any confirmation deadline. It
is not the same as solved, closed or reopened, and it cannot be undone.

## Notifications

The bell in the top bar and **Notifications** list updates about your tickets.
If your organization set up email, you also get emails. They only link back to the
portal: **replying to a notification email does not update your ticket**. Post
your reply in the portal. Turn email or in-app notifications off in **Profile**.

## Keyboard, screen readers and errors

The portal is built for WCAG 2.2 A and AA: you can do everything with the keyboard,
the first Tab stop is **Skip to main content**, and after you follow a link focus
moves to the page heading. Layouts work on phone widths and with larger text, in
both languages. Automated tests check this; a human screen-reader review is still
outstanding ([accessibility](../accessibility.md)).

When something fails, your work is kept:

- **A save or submit is refused** (for example a missing field). Focus moves to
  an error summary at the top of the form that lists each problem as a link to its
  field. Choose a link to jump to that field. Everything you typed stays.
- **The network or the server fails.** The page says so, keeps your text and lets
  you retry. Retrying unchanged input does not create a second ticket, reply or
  decision.
- **Your session expires while you type.** You see "Your session has expired."
  What you typed stays on the page. Choose **Sign in (opens in a new tab)**, sign
  in there, return to the first tab and submit again. See
  [failure recovery](../accessibility.md#failure-recovery) for exactly what is tested.

## If something looks wrong

- "Open the portal at its configured address…": use the link your administrator
  gave you, not an IP address or another hostname.
- "Too many requests": wait a minute and try again.
