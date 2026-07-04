import { useEffect } from 'react'
import { z } from 'zod'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
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

const vmFormSchema = z.object({
  name: z
    .string()
    .min(1, 'Name is required')
    .regex(
      /^[a-zA-Z0-9-_.]+$/,
      'Name can only contain letters, numbers, hyphens, underscores and dots'
    ),
  systemDisk: z.string().min(1, 'System disk path is required'),
  createSystemDisk: optionalString,
  cdrom: optionalString,
  boot: z
    .string()
    .default('hd')
    .transform((value) => value.split(',').filter(Boolean)),
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

const defaultValues: VmFormInput = {
  name: '',
  systemDisk: '',
  createSystemDisk: '',
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
    systemDisk: vm.systemDisk ?? '',
    createSystemDisk: '',
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

  useEffect(() => {
    if (open) {
      form.reset(vm ? summaryToDefaultValues(vm) : defaultValues)
    }
  }, [open, vm, form])

  const graphics = useWatch({ control: form.control, name: 'graphics' })
  const isEdit = mode === 'edit'

  async function handleSubmit(rawValues: VmFormInput) {
    const values: VmFormValues = vmFormSchema.parse(rawValues)
    const { createSystemDisk, ...base } = values

    if (mode === 'create') {
      const input: VmCreateInput = {
        ...base,
        createSystemDisk,
      }
      await onSubmit(input)
    } else {
      const input: VmUpdateInput = base
      await onSubmit(input)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='max-w-2xl'>
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

            <FormField
              control={form.control}
              name='systemDisk'
              render={({ field }) => (
                <FormItem>
                  <FormLabel>System Disk Path</FormLabel>
                  <FormControl>
                    <Input {...field} placeholder='.tmp/disks/sys.qcow2' />
                  </FormControl>
                  <FormDescription>
                    Path to the qcow2 system disk.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            {!isEdit && (
              <FormField
                control={form.control}
                name='createSystemDisk'
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Create System Disk Size</FormLabel>
                    <FormControl>
                      <Input {...field} placeholder='40G (optional)' />
                    </FormControl>
                    <FormDescription>
                      Leave empty if the disk already exists.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            <FormField
              control={form.control}
              name='cdrom'
              render={({ field }) => (
                <FormItem>
                  <FormLabel>CDROM / ISO Path</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder='.tmp/iso/os.iso (optional)'
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <div className='grid gap-4 sm:grid-cols-2'>
              <FormField
                control={form.control}
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
              <FormField
                control={form.control}
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
            </div>

            <div className='grid gap-4 sm:grid-cols-2'>
              <FormField
                control={form.control}
                name='network'
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Network</FormLabel>
                    <FormControl>
                      <Input {...field} placeholder='default' />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name='graphics'
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Graphics</FormLabel>
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
            </div>

            {graphics === 'vnc' && (
              <div className='grid gap-4 sm:grid-cols-2'>
                <FormField
                  control={form.control}
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
                  control={form.control}
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

            <FormField
              control={form.control}
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

            <FormField
              control={form.control}
              name='serialLog'
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Serial Log Path</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      placeholder='.tmp/logs/<name>.serial.log (optional)'
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
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
