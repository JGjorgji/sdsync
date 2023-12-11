# systemd service syncer

This attempts to be more like terraform but for managing your local systemd services, useful if you for example have backups
and don't want to do the handling in the backup scripts but instead have systemd units visible for logging.

It uses minijinja for templating the service files. Here is an example of a restic backup:  

```jinja
# templates/restic-backup.service
[Unit]
Description=Restic backup

[Service]
Type=oneshot
Restart=on-failure
RestartSec=60
LoadCredentialEncrypted=vars:{{ credentials_path}}
ExecStart=/usr/bin/bash -c 'set -a && source "$CREDENTIALS_DIRECTORY/vars" && /usr/bin/restic backup --verbose --one-file-system --tag {{ tag }} {{ backup_path }}'
ExecStartPost=/usr/bin/bash -c 'set -a && source "$CREDENTIALS_DIRECTORY/vars" && /usr/bin/restic forget --verbose --tag {{ tag }} --group-by "paths,tags" --keep-daily {{ retention_days }} --keep-weekly {{ retention_weeks }} --keep-monthly {{ retention_months }} --keep-yearly {{ retention_years }}'
```

Here's how the configuration for the above would look like:  

```yaml
# config.yml
services:
  - template: restic-backup.service
    unit: my-custom-backup.service
    variables:
      credentials_path: /path/to/mybackup.creds
      backup_path: /my/critical/data/
      tag: example
      retention_days: "7"
      retention_weeks: "4"
      retention_months: "6"
      retention_years: "3"
```

You can then add timers as templates and link them to the units, or deploy any other kind of service you want.

This doesn't support loading variables from secrets storage, environment variables, environment files or anything of the like. Instead since we're using systemd services, you can use [systemd-creds](https://systemd.io/CREDENTIALS/) for secret storage.

To run it:  

```sh
sudo sdsync --input config.yml --state state.yml
```

It will attempt to sync the provided files to the systemd services.
