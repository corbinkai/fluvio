{{/*
Expand the name of the chart.
*/}}
{{- define "fluvio.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "fluvio.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "fluvio" }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "fluvio.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels applied to all resources.
*/}}
{{- define "fluvio.labels" -}}
helm.sh/chart: {{ include "fluvio.chart" . }}
{{ include "fluvio.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: fluvio
{{- end }}

{{/*
Base selector labels.
*/}}
{{- define "fluvio.selectorLabels" -}}
app.kubernetes.io/name: {{ include "fluvio.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
SC selector labels.
*/}}
{{- define "fluvio.sc.selectorLabels" -}}
{{ include "fluvio.selectorLabels" . }}
app.kubernetes.io/component: sc
app: fluvio-sc
{{- end }}

{{/*
Namespace GC selector labels.
*/}}
{{- define "fluvio.namespaceGc.selectorLabels" -}}
{{ include "fluvio.selectorLabels" . }}
app.kubernetes.io/component: namespace-gc
{{- end }}

{{/*
Full image reference for fluvio-run.
*/}}
{{- define "fluvio.image" -}}
{{ .Values.image.registry }}/{{ .Values.image.repository }}:{{ .Values.image.tag | default .Chart.AppVersion }}
{{- end }}

{{/*
Service account name.
*/}}
{{- define "fluvio.serviceAccountName" -}}
{{- .Values.serviceAccount.name | default "fluvio" }}
{{- end }}
