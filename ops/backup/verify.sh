#!/bin/sh
# Verifies one backup directory. Without a key it checks the SHA256SUMS of the
# encrypted files (enough to validate an off-host copy). With
# BACKUP_IDENTITY_FILE (the age private key) it also decrypts every stream and
# compares the plaintext SHA-256 values recorded in the manifest.
# usage: verify.sh <backup-dir>
set -eu -o pipefail
. /opt/tikkit-backup/lib.sh

dir=${1:?usage: verify.sh <backup-dir>}
cd "$dir"
sha256sum -c SHA256SUMS > /dev/null || die "checksum mismatch in $dir: the copy is damaged or incomplete"
log info "encrypted files match SHA256SUMS"
[ -n "${BACKUP_IDENTITY_FILE:-}" ] || { log info "no BACKUP_IDENTITY_FILE: decryption checks skipped"; exit 0; }

age -d -i "$BACKUP_IDENTITY_FILE" manifest.txt.age > "${TMPDIR:-/tmp}/manifest.$$" \
  || die "cannot decrypt the manifest: wrong or missing backup decryption key"
manifest=${TMPDIR:-/tmp}/manifest.$$
trap 'rm -f "$manifest"' EXIT
for stream in database.pgdump screenshots.tar config.tar; do
  want=$(sed -n "s/^sha256.$stream=//p" "$manifest")
  have=$(age -d -i "$BACKUP_IDENTITY_FILE" "$stream.age" | sha256sum | cut -d' ' -f1)
  [ -n "$want" ] && [ "$want" = "$have" ] || die "$stream does not match the manifest checksum"
done
files=$(age -d -i "$BACKUP_IDENTITY_FILE" screenshots.tar.age | tar -tf - | wc -l | tr -d ' ')
want=$(sed -n 's/^screenshot_files=//p' "$manifest")
[ "$files" = "$want" ] || die "screenshot archive holds $files files, manifest says $want"
log info "verified $dir: $(sed -n 's/^created_utc=//p' "$manifest"), migration $(sed -n 's/^migration_version=//p' "$manifest"), $files screenshot files"
