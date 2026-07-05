import { useEffect, useMemo, useState } from 'react'
import { z } from 'zod'
import { useFieldArray, useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { Lock, Plus, Trash2 } from 'lucide-react'
import type { VmCreateInput, VmSummary, VmUpdateInput } from '@/lib/api'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Table, TableBody, TableCell, TableRow } from '@/components/ui/table'

const emptyStringToUndefined = (value: string | undefined) =>
  value === '' ? undefined : value

const optionalStringSchema = z.string().optional()
const optionalString = z.preprocess<
  string | undefined,
  typeof optionalStringSchema,
  string | undefined
>(emptyStringToUndefined, optionalStringSchema)

const optionalPortSchema = z.coerce.number<string>().int().optional()
const optionalPort = z.preprocess<
  string | undefined,
  typeof optionalPortSchema,
  string | undefined
>(emptyStringToUndefined, optionalPortSchema)

const vmIoThreadsSchema = z.object({
  count: z.coerce.number<string | number>().int().positive(),
  queues: z.coerce.number<string | number>().int().positive().optional(),
})

const vmDiskIoSchema = z.object({
  mode: z.enum(['threads', 'native', 'io_uring']),
})

const vmFormSchema = z.object({
  name: z
    .string()
    .min(1, 'Name is required')
    .regex(
      /^[a-zA-Z0-9-_.]+$/,
      'Name can only contain letters, numbers, hyphens, underscores and dots'
    ),
  disks: z
    .array(
      z.object({
        type: z.enum(['file', 'block']).optional(),
        path: z.string().min(1, 'Disk path is required'),
        format: z.enum(['raw', 'qcow2']),
        target: z.string().optional(),
        bus: z.enum(['virtio-blk', 'virtio-scsi', 'virtio']).optional(),
        cache: z
          .enum([
            'default',
            'none',
            'writethrough',
            'writeback',
            'directsync',
            'unsafe',
          ])
          .optional(),
        io: vmDiskIoSchema.optional(),
      })
    )
    .min(1, 'At least one disk is required'),
  ioThreads: vmIoThreadsSchema.optional(),
  cdrom: optionalString,
  boot: z
    .string()
    .optional()
    .transform((value) =>
      value ? value.split(',').filter(Boolean) : undefined
    ),
  memoryGiB: z.coerce.number<string | number>().int().min(1).max(512),
  vcpus: z.coerce.number<string | number>().int().min(1).max(128),
  network: z.string().min(1, 'Network is required'),
  graphics: z.enum(['vnc', 'none']),
  vncListen: z.string().transform((value) => value || '127.0.0.1'),
  vncPort: optionalPort,
  serialLog: optionalString,
})

type VmFormInput = z.input<typeof vmFormSchema>
type VmFormValues = z.output<typeof vmFormSchema>

type VmFormDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  mode: 'create' | 'edit'
  vm?: VmSummary | null
  onSubmit: (input: VmCreateInput | VmUpdateInput) => Promise<void>
  isLoading?: boolean
}

type HardwareType =
  | 'resources'
  | 'disks'
  | 'network'
  | 'graphics'
  | 'cdrom'
  | 'serialLog'

type OptionalHardwareType = 'cdrom' | 'serialLog'

type HardwareState = {
  key: string
  enabledOptional: OptionalHardwareType[]
  selectedHardware: HardwareType
}

const OPTIONAL_HARDWARE: { type: OptionalHardwareType; label: string }[] = [
  { type: 'cdrom', label: 'CDROM / ISO' },
  { type: 'serialLog', label: 'Serial Log' },
]

const defaultValues: VmFormInput = {
  name: '',
  disks: [{ type: 'file', path: '', format: 'qcow2', bus: 'virtio-blk' }],
  cdrom: '',
  boot: 'hd',
  memoryGiB: 4,
  vcpus: 2,
  network: 'default',
  graphics: 'vnc',
  vncListen: '127.0.0.1',
  vncPort: '',
  serialLog: '',
}

function summaryToDefaultValues(vm: VmSummary): VmFormInput {
  return {
    name: vm.name,
    ioThreads: vm.ioThreads ?? undefined,
    disks: vm.disks?.length
      ? vm.disks
      : [{ type: 'file', path: '', format: 'qcow2', bus: 'virtio-blk' }],
    cdrom: vm.cdrom ?? '',
    boot: vm.boot?.join(',') ?? 'hd',
    memoryGiB: vm.memoryMiB ? Math.round(vm.memoryMiB / 1024) : 4,
    vcpus: vm.vcpus ?? 2,
    network: vm.network ?? 'default',
    graphics: vm.graphics ?? 'vnc',
    vncListen: vm.vncListen ?? '127.0.0.1',
    vncPort: vm.vncPort ? String(vm.vncPort) : '',
    serialLog: vm.serialLog ?? '',
  }
}

function diskSummary(disks: VmFormInput['disks'] | undefined): string {
  if (!disks?.length) return 'Not set'
  const firstPath = disks[0]?.path || 'Not set'
  return disks.length === 1 ? firstPath : `${disks.length} disks`
}

function detectOptionalHardware(vm: VmSummary): OptionalHardwareType[] {
  const enabled: OptionalHardwareType[] = []
  if (vm.cdrom) enabled.push('cdrom')
  if (vm.serialLog) enabled.push('serialLog')
  return enabled
}

function initialHardwareState(
  key: string,
  vm?: VmSummary | null
): HardwareState {
  return {
    key,
    enabledOptional: vm ? detectOptionalHardware(vm) : [],
    selectedHardware: 'resources',
  }
}

export function VmFormDialog({
  open,
  onOpenChange,
  mode,
  vm,
  onSubmit,
  isLoading,
}: VmFormDialogProps) {
  const form = useForm<VmFormInput>({
    resolver: zodResolver(vmFormSchema, undefined, { raw: true }),
    defaultValues,
  })
  const hardwareKey = open ? `${mode}:${vm?.name ?? 'new'}` : 'closed'
  const [hardwareState, setHardwareState] = useState<HardwareState>(() =>
    initialHardwareState(hardwareKey, vm)
  )
  const currentHardware =
    hardwareState.key === hardwareKey
      ? hardwareState
      : initialHardwareState(hardwareKey, vm)
  const { enabledOptional, selectedHardware } = currentHardware

  useEffect(() => {
    if (open) {
      form.reset(vm ? summaryToDefaultValues(vm) : defaultValues)
    }
  }, [open, vm, form])

  const [
    graphics,
    vcpus,
    memoryGiB,
    disks,
    network,
    cdrom,
    serialLog,
    vncListen,
    vncPort,
  ] = useWatch({
    control: form.control,
    name: [
      'graphics',
      'vcpus',
      'memoryGiB',
      'disks',
      'network',
      'cdrom',
      'serialLog',
      'vncListen',
      'vncPort',
    ],
  })
  const isEdit = mode === 'edit'

  function addOptional(type: OptionalHardwareType) {
    setHardwareState({
      key: hardwareKey,
      enabledOptional: [...enabledOptional, type],
      selectedHardware: type,
    })
    if (type === 'cdrom') form.setValue('cdrom', '')
    if (type === 'serialLog') form.setValue('serialLog', '')
  }

  function removeOptional(type: OptionalHardwareType) {
    setHardwareState({
      key: hardwareKey,
      enabledOptional: enabledOptional.filter((t) => t !== type),
      selectedHardware: 'resources',
    })
    if (type === 'cdrom') form.setValue('cdrom', '')
    if (type === 'serialLog') form.setValue('serialLog', '')
  }

  function selectHardware(type: HardwareType) {
    setHardwareState({
      key: hardwareKey,
      enabledOptional,
      selectedHardware: type,
    })
  }

  async function handleSubmit(rawValues: VmFormInput) {
    const cleaned: VmFormInput = { ...rawValues }
    if (!enabledOptional.includes('cdrom')) cleaned.cdrom = ''
    if (!enabledOptional.includes('serialLog')) cleaned.serialLog = ''

    const values: VmFormValues = vmFormSchema.parse(cleaned)
    if (mode === 'create') {
      const input: VmCreateInput = values
      await onSubmit(input)
    } else {
      const input: VmUpdateInput = values
      await onSubmit(input)
    }
  }

  const hardwareList = useMemo(
    () => [
      {
        type: 'resources' as const,
        label: 'CPU & Memory',
        summary: `${vcpus ?? 2} vCPU(s) · ${memoryGiB ?? 4} GiB`,
        locked: true,
      },
      {
        type: 'disks' as const,
        label: 'Disks',
        summary: diskSummary(disks),
        locked: true,
      },
      {
        type: 'network' as const,
        label: 'Network',
        summary: network || 'Not set',
        locked: true,
      },
      {
        type: 'graphics' as const,
        label: 'Graphics',
        summary:
          graphics === 'vnc'
            ? `VNC @ ${vncListen || '127.0.0.1'}${vncPort ? `:${vncPort}` : ''}`
            : 'None',
        locked: true,
      },
      ...(enabledOptional.includes('cdrom')
        ? [
            {
              type: 'cdrom' as const,
              label: 'CDROM / ISO',
              summary: cdrom || 'Not set',
              locked: false,
            },
          ]
        : []),
      ...(enabledOptional.includes('serialLog')
        ? [
            {
              type: 'serialLog' as const,
              label: 'Serial Log',
              summary: serialLog || 'Not set',
              locked: false,
            },
          ]
        : []),
    ],
    [
      vcpus,
      memoryGiB,
      disks,
      network,
      graphics,
      vncListen,
      vncPort,
      cdrom,
      serialLog,
      enabledOptional,
    ]
  )

  const availableOptional = OPTIONAL_HARDWARE.filter(
    (h) => !enabledOptional.includes(h.type)
  )

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='w-[90vw] max-w-5xl'>
        <DialogHeader>
          <DialogTitle>
            {isEdit ? 'Edit Virtual Machine' : 'Create Virtual Machine'}
          </DialogTitle>
          <DialogDescription>
            {isEdit
              ? 'Update the libvirt domain definition.'
              : 'Define a new libvirt domain. The VM will not be started automatically.'}
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            id='vm-form'
            onSubmit={form.handleSubmit(handleSubmit)}
            className='grid gap-4 py-2'
          >
            <FormField
              control={form.control}
              name='name'
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Name</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      disabled={isEdit}
                      placeholder='install-os'
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <div className='grid gap-4 md:grid-cols-[18rem_1fr]'>
              <div className='flex min-h-[28rem] flex-col gap-3'>
                <div className='rounded-lg border'>
                  <Table>
                    <TableBody>
                      {hardwareList.map((hw) => (
                        <TableRow
                          key={hw.type}
                          data-selected={selectedHardware === hw.type}
                          className='cursor-pointer data-[selected=true]:bg-muted'
                          onClick={() => selectHardware(hw.type)}
                        >
                          <TableCell className='py-3'>
                            <div className='flex items-center justify-between gap-2'>
                              <div>
                                <div className='flex items-center gap-1.5 font-medium'>
                                  {hw.label}
                                  {hw.locked && (
                                    <Lock className='size-3 text-muted-foreground' />
                                  )}
                                </div>
                                <div className='max-w-[14rem] truncate text-xs text-muted-foreground'>
                                  {hw.summary}
                                </div>
                              </div>
                              {!hw.locked && (
                                <Button
                                  type='button'
                                  variant='ghost'
                                  size='icon'
                                  className='size-6'
                                  onClick={(e) => {
                                    e.stopPropagation()
                                    removeOptional(
                                      hw.type as OptionalHardwareType
                                    )
                                  }}
                                >
                                  <Trash2 className='size-3.5 text-destructive' />
                                </Button>
                              )}
                            </div>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant='outline'
                      size='sm'
                      disabled={availableOptional.length === 0}
                    >
                      <Plus className='size-4' />
                      Add hardware
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align='start'>
                    {availableOptional.map((h) => (
                      <DropdownMenuItem
                        key={h.type}
                        onClick={() => addOptional(h.type)}
                      >
                        {h.label}
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>

              <div className='min-h-[28rem] rounded-lg border bg-card p-4'>
                <div className='mb-4 text-sm font-medium text-muted-foreground'>
                  Edit{' '}
                  {hardwareList.find((h) => h.type === selectedHardware)?.label}
                </div>
                <HardwareEditPanel
                  type={selectedHardware}
                  control={form.control}
                />
              </div>
            </div>
          </form>
        </Form>
        <DialogFooter>
          <Button
            variant='outline'
            onClick={() => onOpenChange(false)}
            disabled={isLoading}
          >
            Cancel
          </Button>
          <Button type='submit' form='vm-form' disabled={isLoading}>
            {isLoading ? 'Saving...' : isEdit ? 'Save Changes' : 'Create VM'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function HardwareEditPanel({
  type,
  control,
}: {
  type: HardwareType
  control: ReturnType<typeof useForm<VmFormInput>>['control']
}) {
  switch (type) {
    case 'resources':
      return (
        <div className='grid gap-4 sm:grid-cols-2'>
          <FormField
            control={control}
            name='vcpus'
            render={({ field }) => (
              <FormItem>
                <FormLabel>vCPUs</FormLabel>
                <FormControl>
                  <Input {...field} type='number' min={1} />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={control}
            name='memoryGiB'
            render={({ field }) => (
              <FormItem>
                <FormLabel>Memory (GiB)</FormLabel>
                <FormControl>
                  <Input {...field} type='number' min={1} />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
        </div>
      )
    case 'disks':
      return <DisksEditPanel control={control} />
    case 'network':
      return (
        <FormField
          control={control}
          name='network'
          render={({ field }) => (
            <FormItem>
              <FormLabel>Network Name</FormLabel>
              <FormControl>
                <Input {...field} placeholder='default' />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
      )
    case 'graphics':
      return <GraphicsEditPanel control={control} />
    case 'cdrom':
      return (
        <FormField
          control={control}
          name='cdrom'
          render={({ field }) => (
            <FormItem>
              <FormLabel>ISO Path</FormLabel>
              <FormControl>
                <Input {...field} placeholder='.tmp/iso/os.iso' />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
      )
    case 'serialLog':
      return (
        <FormField
          control={control}
          name='serialLog'
          render={({ field }) => (
            <FormItem>
              <FormLabel>Log Path</FormLabel>
              <FormControl>
                <Input {...field} placeholder='.tmp/logs/<name>.serial.log' />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
      )
    default:
      return null
  }
}

function DisksEditPanel({
  control,
}: {
  control: ReturnType<typeof useForm<VmFormInput>>['control']
}) {
  const { fields, append, remove } = useFieldArray({
    control,
    name: 'disks',
  })

  return (
    <div className='grid gap-4'>
      {fields.map((disk, index) => (
        <div key={disk.id} className='grid gap-3 rounded-lg border p-3'>
          <div className='flex items-center justify-between gap-2'>
            <div className='text-sm font-medium'>Disk {index + 1}</div>
            {fields.length > 1 && (
              <Button
                type='button'
                variant='ghost'
                size='icon'
                className='size-7'
                onClick={() => remove(index)}
              >
                <Trash2 className='size-4 text-destructive' />
              </Button>
            )}
          </div>
          <div className='grid gap-4 sm:grid-cols-[1fr_10rem]'>
            <FormField
              control={control}
              name={`disks.${index}.path` as const}
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Path</FormLabel>
                  <FormControl>
                    <Input {...field} placeholder='.tmp/disks/vm.qcow2' />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={control}
              name={`disks.${index}.format` as const}
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Format</FormLabel>
                  <Select onValueChange={field.onChange} value={field.value}>
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      <SelectItem value='qcow2'>qcow2</SelectItem>
                      <SelectItem value='raw'>raw</SelectItem>
                    </SelectContent>
                  </Select>
                  <FormMessage />
                </FormItem>
              )}
            />
          </div>
        </div>
      ))}
      <Button
        type='button'
        variant='outline'
        size='sm'
        onClick={() =>
          append({ type: 'file', path: '', format: 'qcow2', bus: 'virtio-blk' })
        }
      >
        <Plus className='size-4' />
        Add Disk
      </Button>
      <FormField
        control={control}
        name='boot'
        render={({ field }) => (
          <FormItem>
            <FormLabel>Boot Order</FormLabel>
            <Select onValueChange={field.onChange} value={field.value}>
              <FormControl>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
              </FormControl>
              <SelectContent>
                <SelectItem value='hd'>Hard Disk</SelectItem>
                <SelectItem value='cdrom,hd'>CDROM, Hard Disk</SelectItem>
                <SelectItem value='hd,cdrom'>Hard Disk, CDROM</SelectItem>
              </SelectContent>
            </Select>
            <FormMessage />
          </FormItem>
        )}
      />
    </div>
  )
}

function GraphicsEditPanel({
  control,
}: {
  control: ReturnType<typeof useForm<VmFormInput>>['control']
}) {
  const graphics = useWatch({ control, name: 'graphics' })

  return (
    <div className='grid gap-4'>
      <FormField
        control={control}
        name='graphics'
        render={({ field }) => (
          <FormItem>
            <FormLabel>Graphics Device</FormLabel>
            <Select onValueChange={field.onChange} value={field.value}>
              <FormControl>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
              </FormControl>
              <SelectContent>
                <SelectItem value='vnc'>VNC</SelectItem>
                <SelectItem value='none'>None</SelectItem>
              </SelectContent>
            </Select>
            <FormMessage />
          </FormItem>
        )}
      />
      {graphics === 'vnc' && (
        <div className='grid gap-4 sm:grid-cols-2'>
          <FormField
            control={control}
            name='vncListen'
            render={({ field }) => (
              <FormItem>
                <FormLabel>VNC Listen Address</FormLabel>
                <FormControl>
                  <Input {...field} placeholder='127.0.0.1' />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={control}
            name='vncPort'
            render={({ field }) => (
              <FormItem>
                <FormLabel>VNC Port</FormLabel>
                <FormControl>
                  <Input {...field} placeholder='Auto (optional)' />
                </FormControl>
                <FormDescription>
                  Leave empty for auto-assignment.
                </FormDescription>
                <FormMessage />
              </FormItem>
            )}
          />
        </div>
      )}
    </div>
  )
}
