import type { VmMetrics, VmSummary } from '@/lib/api'

export type VmMetricSnapshot = VmMetrics

export type VmRuntimeMetrics = {
  cpu: string
  memory: string
  tx: string
  rx: string
}

export function vmMetricsByName(vms: VmSummary[]): Map<string, VmMetricSnapshot> {
  return new Map(
    vms
      .filter((vm): vm is VmSummary & { metrics: VmMetricSnapshot } => Boolean(vm.metrics))
      .map((vm) => [vm.name, vm.metrics])
  )
}

export function vmRuntimeMetrics(vm?: VmSummary, previous?: VmMetricSnapshot): VmRuntimeMetrics {
  if (!vm?.metrics) {
    return emptyRuntimeMetrics()
  }

  return {
    cpu: formatCpuUsage(vm, previous),
    memory: formatMemoryUsage(vm.metrics),
    tx: formatByteRate(vm.metrics.txBytes, previous?.txBytes, vm.metrics.sampledAtMs, previous?.sampledAtMs),
    rx: formatByteRate(vm.metrics.rxBytes, previous?.rxBytes, vm.metrics.sampledAtMs, previous?.sampledAtMs),
  }
}

function emptyRuntimeMetrics(): VmRuntimeMetrics {
  return { cpu: '-', memory: '-', tx: '-', rx: '-' }
}

function formatCpuUsage(vm: VmSummary, previous?: VmMetricSnapshot): string {
  if (!previous || !vm.metrics || !vm.vcpus) {
    return '-'
  }

  const elapsedMs = vm.metrics.sampledAtMs - previous.sampledAtMs
  const cpuDeltaNs = vm.metrics.cpuTimeNs - previous.cpuTimeNs
  if (elapsedMs <= 0 || cpuDeltaNs < 0) {
    return '-'
  }

  const usage = (cpuDeltaNs / (elapsedMs * 1_000_000) / vm.vcpus) * 100
  return `${Math.min(Math.max(usage, 0), 100).toFixed(1)}%`
}

function formatMemoryUsage(metrics: VmMetricSnapshot): string {
  if (metrics.memoryTotalMiB <= 0) {
    return '-'
  }

  const percent = (metrics.memoryUsedMiB / metrics.memoryTotalMiB) * 100
  return `${percent.toFixed(1)}%`
}

function formatByteRate(current?: number, previous?: number, currentAtMs?: number, previousAtMs?: number): string {
  if (current === undefined || previous === undefined || currentAtMs === undefined || previousAtMs === undefined) {
    return '-'
  }

  const elapsedSeconds = (currentAtMs - previousAtMs) / 1000
  const byteDelta = current - previous
  if (elapsedSeconds <= 0 || byteDelta < 0) {
    return '-'
  }

  return `${(byteDelta / elapsedSeconds / 1024 / 1024).toFixed(2)} MiB/s`
}
