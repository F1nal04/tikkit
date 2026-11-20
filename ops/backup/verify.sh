#!/bin/sh
# Verifies one backup directory. Without a key it checks that SHA256SUMS lists
# exactly the four expected encrypted files and that each matches (enough to
# validate an off-host copy at its destination). With BACKUP_IDENTITY_FILE (the
# age private key) it also decrypts every stream and compares the plaintext
# SHA-256 values recorded in the manifest. Nothing decrypted touches the disk.
# usage: verify.sh <backup-dir>
set -eu -o pipefail
umask 077
. /opt/tikkit-backup/lib.sh

dir=${1:?usage: verify.sh <backup-dir>}
cd "$dir"
reason=$(checksums_ok) || die "$reason in $dir: the copy is damaged or incomplete"
log info "encrypted files match SHA256SUMS"
[ ! -e DEGRADED ] || log warn "this backup is degraded ($(cat DEGRADED)): some screenshot files were already missing at backup time"
[ -n "${BACKUP_IDENTITY_FILE:-}" ] || { log info "no BACKUP_IDENTITY_FILE: decryption checks skipped"; exit 0; }

manifest=$(age -d -i "$BACKUP_IDENTITY_FILE" manifest.txt.age) \
  || die "cannot decrypt the manifest: wrong or missing backup decryption key"
for stream in database.pgdump screenshots.tar config.tar; do
  want=$(printf '%s\n' "$manifest" | sed -n "s/^sha256.$stream=//p")
  have=$(age -d -i "$BACKUP_IDENTITY_FILE" "$stream.age" | sha256sum | cut -d' ' -f1)
  [ -n "$want" ] && [ "$want" = "$have" ] || die "$stream does not match the manifest checksum"
done
files=$(age -d -i "$BACKUP_IDENTITY_FILE" screenshots.tar.age | tar -tf - | wc -l | tr -d ' ')
want=$(printf '%s\n' "$manifest" | sed -n 's/^screenshot_files=//p')
[ "$files" = "$want" ] || die "screenshot archive holds $files files, manifest says $want"
log info "verified $dir: $(printf '%s\n' "$manifest" | sed -n 's/^created_utc=//p'), migration $(printf '%s\n' "$manifest" | sed -n 's/^migration_version=//p'), $files screenshot files"
