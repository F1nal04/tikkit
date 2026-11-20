# Backup and restore tooling for the reference Compose deployment: POSIX shell,
# the PostgreSQL 17 client tools and age. No other runtime. The image holds no
# private key: it encrypts to public recipients only.
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add --no-cache age postgresql17-client busybox-extras \
    && addgroup -g 10001 tikkit \
    && adduser -D -H -u 10001 -G tikkit tikkit \
    && install -d -m 0700 -o tikkit -g tikkit /backups \
    && install -d -m 0755 /config
COPY ops/backup/lib.sh ops/backup/backup.sh ops/backup/verify.sh ops/backup/schedule.sh ops/backup/entrypoint.sh /opt/tikkit-backup/
RUN chmod 0755 /opt/tikkit-backup/*.sh
# Same UID as the API image, which owns the private screenshot volume.
USER 10001:10001
ENTRYPOINT ["/opt/tikkit-backup/entrypoint.sh"]
CMD ["schedule"]
