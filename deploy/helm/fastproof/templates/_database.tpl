{{- define "fastproof.k8sMiB" -}}
{{- $s := toString . -}}
{{- $num := regexFind "^[0-9]+(\\.[0-9]+)?" $s -}}
{{- $unit := trimPrefix $num $s -}}
{{- $factors := dict "" (divf 1 1048576) "k" (divf 1000 1048576) "M" (divf 1000000 1048576) "G" (divf 1000000000 1048576) "Ki" (divf 1 1024) "Mi" 1.0 "Gi" 1024.0 "Ti" 1048576.0 -}}
{{- if or (not $num) (not (hasKey $factors $unit)) -}}
{{- fail (printf "cannot read the memory quantity %q" $s) -}}
{{- end -}}
{{- mulf (float64 $num) (index $factors $unit) | floor | int -}}
{{- end -}}

{{- define "fastproof.pgMiB" -}}
{{- $s := toString .value -}}
{{- $num := regexFind "^[0-9]+" $s -}}
{{- $unit := trimPrefix $num $s | trim -}}
{{- $factors := dict "" (divf .baseKiB 1024) "kB" (divf 1 1024) "MB" 1.0 "GB" 1024.0 "TB" 1048576.0 -}}
{{- if or (not $num) (not (hasKey $factors $unit)) -}}
{{- fail (printf "cannot read the PostgreSQL memory setting %q (use kB, MB or GB)" $s) -}}
{{- end -}}
{{- mulf (float64 $num) (index $factors $unit) | floor | int -}}
{{- end -}}

{{- define "fastproof.databaseConnections" -}}
{{- $pool := int .Values.database.maxConnections -}}
{{- $total := 0 -}}
{{- range (include "fastproof.workloads" . | fromJsonArray) -}}
{{- $replicas := int .replicaCount -}}
{{- if .autoscaling.enabled -}}
{{- $replicas = int .autoscaling.maxReplicas -}}
{{- end -}}
{{- $total = add $total (mul $replicas (add1 $pool)) -}}
{{- end -}}
{{- if eq .Values.migrations.mode "job" -}}
{{- $total = add1 $total -}}
{{- end -}}
{{- $total -}}
{{- end -}}

{{- define "fastproof.validateDatabaseSizing" -}}
{{- if .Values.database.sizingCheck -}}
{{- $needed := int (include "fastproof.databaseConnections" .) -}}
{{- if .Values.postgresql.enabled -}}
{{- $pg := .Values.postgresql.config.postgresql -}}
{{- $max := int $pg.max_connections -}}
{{- if gt (add $needed 3) $max -}}
{{- fail (printf "the FastProof pods can open %d PostgreSQL connections (database.maxConnections + 1 per pod at the most replicas, + 1 for migrations) but the bundled PostgreSQL allows %d (postgresql.config.postgresql.max_connections, 3 kept for superusers): raise max_connections together with the memory limit, or lower database.maxConnections (docs/operations/scaling.md)" $needed $max) -}}
{{- end -}}
{{- $limit := dig "resources" "limits" "memory" "" .Values.postgresql -}}
{{- if $limit -}}
{{- $limitMiB := int (include "fastproof.k8sMiB" $limit) -}}
{{- $shared := int (include "fastproof.pgMiB" (dict "value" $pg.shared_buffers "baseKiB" 8)) -}}
{{- $maintenance := int (include "fastproof.pgMiB" (dict "value" $pg.maintenance_work_mem "baseKiB" 1)) -}}
{{- $estimate := add $shared (mul 8 $needed) (mul 3 $maintenance) 128 -}}
{{- if gt $estimate $limitMiB -}}
{{- fail (printf "the bundled PostgreSQL needs about %dMi (shared_buffers %dMi + 8Mi for each of %d connections + 3 autovacuum workers x maintenance_work_mem %dMi + 128Mi) but its memory limit is %s: raise postgresql.resources.limits.memory, lower database.maxConnections or set database.sizingCheck=false (docs/operations/scaling.md)" $estimate $shared $needed $maintenance $limit) -}}
{{- end -}}
{{- end -}}
{{- else if .Values.cnpg.enabled -}}
{{- $max := int (dig "cluster" "postgresql" "parameters" "max_connections" "100" .Values.cnpg) -}}
{{- if gt (add $needed 3) $max -}}
{{- fail (printf "the FastProof pods can open %d PostgreSQL connections (database.maxConnections + 1 per pod at the most replicas, + 1 for migrations) but the CloudNativePG cluster allows %d (cnpg.cluster.postgresql.parameters.max_connections, 3 kept for superusers): raise max_connections or lower database.maxConnections (docs/operations/scaling.md)" $needed $max) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
