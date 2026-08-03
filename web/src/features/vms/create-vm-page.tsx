import { useState } from 'react'
import { z } from 'zod'
import axios from 'axios'
import { useFieldArray, useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import {
  ArrowLeft,
  ArrowRight,
  Check,
  Disc3,
  HardDrive,
  Plus,
  Trash2,
} from 'lucide-react'
import {
  createVm,
  getDisks,
  getIsos,
  getNetworks,
  type VmCreateInput,
} from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
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
import { Switch } from '@/components/ui/switch'
import { Header } from '@/components/layout/header'
import { Main } from '@/components/layout/main'
import { Search } from '@/components/search'
import { ThemeSwitch } from '@/components/theme-switch'

const createVmSchema = z.object({
  name: z
    .string()
    .min(1, 'Name is required')
    .regex(
      /^[a-zA-Z0-9-_.]+$/,
      'Use only letters, numbers, hyphens, underscores, and dots'
    ),
  machineType: z.enum(['standard', 'microvm']),
  vcpus: z.coerce.number<string | number>().int().min(1).max(128),
  memoryMib: z.coerce.number<string | number>().int().min(256).max(524_288),
  disks: z
    .array(
      z.object({
        imageId: z.string().min(1, 'Select a disk'),
        format: z.enum(['qcow2', 'raw']),
        bus: z.enum(['virtio-blk', 'virtio-scsi']),
      })
    )
    .min(1, 'At least one disk is required')
    .refine(
      (disks) =>
        new Set(disks.map((disk) => disk.imageId)).size === disks.length,
      'Each disk can only be attached once'
    ),
  networkId: z.string().min(1, 'Select a network'),
  cdroms: z
    .array(
      z.object({
        id: z
          .string()
          .min(1, 'Enter a tray ID')
          .max(48)
          .regex(
            /^[a-zA-Z0-9._-]+$/,
            'Use letters, numbers, dot, underscore, or hyphen'
          ),
        mediaId: z.string().nullable(),
      })
    )
    .refine(
      (cdroms) =>
        new Set(cdroms.map((cdrom) => cdrom.id)).size === cdroms.length,
      'Each tray ID must be unique'
    ),
  graphics: z.enum(['vnc', 'none']),
  serialLog: z.boolean(),
})

type CreateVmFormInput = z.input<typeof createVmSchema>
type CreateVmFormValues = z.output<typeof createVmSchema>

const defaultValues: CreateVmFormInput = {
  name: '',
  machineType: 'standard',
  vcpus: 2,
  memoryMib: 4096,
  disks: [{ imageId: '', format: 'qcow2', bus: 'virtio-blk' }],
  networkId: 'default',
  cdroms: [{ id: 'installer', mediaId: null }],
  graphics: 'vnc',
  serialLog: true,
}

const sections = [
  ['identity', '01', 'Identity'],
  ['compute', '02', 'Compute'],
  ['storage', '03', 'Storage'],
  ['network', '04', 'Network'],
  ['access', '05', 'Access'],
] as const

export function CreateVmPage() {
  const [source, setSource] = useState<'choose' | 'disk'>('choose')

  return (
    <>
      <Header fixed>
        <Search placeholder='Go to...' />
        <div className='ms-auto'>
          <ThemeSwitch />
        </div>
      </Header>
      <Main>
        <Button variant='ghost' size='sm' asChild className='-ms-2 mb-6'>
          <Link to='/vms'>
            <ArrowLeft className='size-4' />
            Virtual machines
          </Link>
        </Button>
        {source === 'choose' ? (
          <SourceChoice onUseDisk={() => setSource('disk')} />
        ) : (
          <ManagedDiskForm onBack={() => setSource('choose')} />
        )}
      </Main>
    </>
  )
}

function SourceChoice({ onUseDisk }: { onUseDisk: () => void }) {
  return (
    <div className='mx-auto max-w-5xl'>
      <div className='mb-10 border-b border-border pb-8'>
        <p className='mb-3 font-mono text-[0.625rem] tracking-[0.17em] text-muted-foreground uppercase'>
          New virtual machine
        </p>
        <h1 className='text-4xl font-medium tracking-[-0.045em] sm:text-5xl'>
          Choose a source
        </h1>
        <p className='mt-3 max-w-xl text-sm leading-6 text-muted-foreground'>
          Install a fresh Fedora system or define a domain around an existing
          managed disk.
        </p>
      </div>
      <div className='grid gap-4 md:grid-cols-2'>
        <SourceButton
          icon={Disc3}
          index='01'
          title='Automated Fedora install'
          description='Create a new disk with an unattended, persistent installation job.'
          install
        />
        <SourceButton
          icon={HardDrive}
          index='02'
          title='Use existing disk'
          description='Attach a managed virtual disk and define a powered-off libvirt domain.'
          onClick={onUseDisk}
        />
      </div>
    </div>
  )
}

function SourceButton({
  icon: Icon,
  index,
  title,
  description,
  install = false,
  onClick,
}: {
  icon: React.ElementType
  index: string
  title: string
  description: string
  install?: boolean
  onClick?: () => void
}) {
  const content = (
    <>
      <div className='flex w-full flex-col p-6 sm:p-7'>
        <div className='flex items-center justify-between'>
          <span className='font-mono text-[0.625rem] tracking-[0.15em] text-muted-foreground'>
            {index}
          </span>
          <Icon className='size-5 text-muted-foreground' />
        </div>
        <div className='mt-auto pt-12'>
          <span className='flex items-center justify-between text-lg font-medium'>
            {title}
            <ArrowRight className='size-4 transition-transform group-hover:translate-x-1' />
          </span>
          <span className='mt-2 block max-w-sm text-sm leading-5 text-muted-foreground'>
            {description}
          </span>
        </div>
      </div>
    </>
  )
  const className =
    'group h-auto min-h-52 items-stretch justify-start whitespace-normal rounded-none p-0 text-start'
  if (install) {
    return (
      <Button variant='outline' className={className} asChild>
        <Link to='/installations' search={{ create: true }}>
          {content}
        </Link>
      </Button>
    )
  }
  return (
    <Button variant='outline' className={className} onClick={onClick}>
      {content}
    </Button>
  )
}

function ManagedDiskForm({ onBack }: { onBack: () => void }) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [serverError, setServerError] = useState<string | null>(null)
  const form = useForm<CreateVmFormInput>({
    resolver: zodResolver(createVmSchema, undefined, { raw: true }),
    defaultValues,
  })
  const disks = useFieldArray({ control: form.control, name: 'disks' })
  const cdroms = useFieldArray({ control: form.control, name: 'cdroms' })
  const values = useWatch({ control: form.control })
  const managedDisks = useQuery({
    queryKey: ['resources', 'disks'],
    queryFn: getDisks,
  })
  const isos = useQuery({
    queryKey: ['resources', 'isos'],
    queryFn: getIsos,
  })
  const networks = useQuery({ queryKey: ['networks'], queryFn: getNetworks })
  const create = useMutation({
    mutationFn: createVm,
    onSuccess: async (vm) => {
      await queryClient.invalidateQueries({ queryKey: ['vms'] })
      navigate({ to: '/vms/$name', params: { name: vm.name } })
    },
    onError: (error) => {
      const detail = axios.isAxiosError(error)
        ? error.response?.data?.detail
        : null
      setServerError(
        typeof detail === 'string' ? detail : 'The VM could not be defined.'
      )
    },
  })

  function submit(rawValues: CreateVmFormInput) {
    setServerError(null)
    const values: CreateVmFormValues = createVmSchema.parse(rawValues)
    const input: VmCreateInput = {
      name: values.name,
      machineType: values.machineType,
      resources: { vcpus: values.vcpus, memoryMib: values.memoryMib },
      disks: values.disks,
      networkId:
        values.machineType === 'standard' ? values.networkId : undefined,
      cdroms: values.cdroms,
      console: { graphics: values.graphics, serialLog: values.serialLog },
    }
    create.mutate(input)
  }

  const selectedDiskIds = new Set(
    values.disks?.map((disk) => disk?.imageId).filter(Boolean)
  )

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(submit)}>
        <div className='mb-8 flex flex-col gap-5 border-b border-border pb-7 sm:flex-row sm:items-end sm:justify-between'>
          <div>
            <button
              type='button'
              onClick={onBack}
              className='mb-3 font-mono text-[0.625rem] tracking-[0.17em] text-muted-foreground uppercase hover:text-foreground'
            >
              Existing managed disk · Change source
            </button>
            <h1 className='text-3xl font-medium tracking-[-0.04em] sm:text-4xl'>
              Define virtual machine
            </h1>
            <p className='mt-2 max-w-xl text-sm text-muted-foreground'>
              The disk stays in place. The new domain is created powered off.
            </p>
          </div>
        </div>

        <div className='grid gap-10 xl:grid-cols-[9rem_minmax(0,1fr)_20rem]'>
          <nav className='hidden xl:block'>
            <div className='sticky top-20 grid gap-1'>
              {sections.map(([id, index, label]) => (
                <a
                  key={id}
                  href={`#${id}`}
                  className='flex gap-3 px-2 py-2 text-xs text-muted-foreground hover:text-foreground'
                >
                  <span className='font-mono'>{index}</span>
                  {label}
                </a>
              ))}
            </div>
          </nav>

          <div className='grid min-w-0 gap-10'>
            <FormSection
              id='identity'
              index='01'
              title='Identity'
              description='Name the libvirt domain.'
            >
              <FormField
                control={form.control}
                name='name'
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Domain name</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        autoFocus
                        placeholder='fedora-dev'
                        className='font-mono'
                      />
                    </FormControl>
                    <FormDescription>
                      Letters, numbers, dots, underscores, and hyphens.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name='machineType'
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Machine profile</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value='standard'>Standard VM</SelectItem>
                        <SelectItem value='microvm'>MicroVM</SelectItem>
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      MicroVM uses a minimal QEMU machine and user-mode NAT.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </FormSection>

            <FormSection
              id='compute'
              index='02'
              title='Compute'
              description='Resources reserved for the guest.'
            >
              <div className='grid gap-5 sm:grid-cols-2'>
                <FormField
                  control={form.control}
                  name='vcpus'
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>vCPUs</FormLabel>
                      <FormControl>
                        <Input {...field} type='number' min={1} max={128} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name='memoryMib'
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Memory MiB</FormLabel>
                      <FormControl>
                        <Input {...field} type='number' min={256} step={256} />
                      </FormControl>
                      <FormDescription>
                        {formatMemory(Number(field.value))}
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            </FormSection>

            <FormSection
              id='storage'
              index='03'
              title='Storage'
              description='Attach existing writable virtual disks from the managed root.'
            >
              <div className='grid gap-4'>
                {disks.fields.map((disk, index) => (
                  <div
                    key={disk.id}
                    className='grid gap-4 border border-border bg-card p-4'
                  >
                    <div className='flex items-center justify-between'>
                      <span className='font-mono text-xs'>
                        Disk {String(index + 1).padStart(2, '0')}
                      </span>
                      {disks.fields.length > 1 && (
                        <Button
                          type='button'
                          variant='ghost'
                          size='icon'
                          aria-label={`Remove disk ${index + 1}`}
                          onClick={() => disks.remove(index)}
                        >
                          <Trash2 className='size-4 text-destructive' />
                        </Button>
                      )}
                    </div>
                    <FormField
                      control={form.control}
                      name={`disks.${index}.imageId`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>Managed disk</FormLabel>
                          <Select
                            value={field.value}
                            onValueChange={(value) => {
                              field.onChange(value)
                              const image = managedDisks.data?.find(
                                (disk) => disk.id === value
                              )
                              form.setValue(
                                `disks.${index}.format`,
                                image?.format ?? 'qcow2'
                              )
                            }}
                          >
                            <FormControl>
                              <SelectTrigger>
                                <SelectValue
                                  placeholder={
                                    managedDisks.isPending
                                      ? 'Loading disks…'
                                      : 'Select disk'
                                  }
                                />
                              </SelectTrigger>
                            </FormControl>
                            <SelectContent>
                              {managedDisks.data?.map((disk) => (
                                <SelectItem
                                  key={disk.id}
                                  value={disk.id}
                                  disabled={
                                    disk.status !== 'ready' ||
                                    disk.attachments.length > 0 ||
                                    Boolean(disk.reservedByJobId) ||
                                    (selectedDiskIds.has(disk.id) &&
                                      disk.id !== field.value)
                                  }
                                >
                                  {disk.id} ·{' '}
                                  {formatBytes(
                                    disk.virtualSizeBytes ?? disk.sizeBytes
                                  )}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                    <div className='grid gap-4 sm:grid-cols-2'>
                      <FormField
                        control={form.control}
                        name={`disks.${index}.format`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Format</FormLabel>
                            <Select value={field.value} disabled>
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
                      <FormField
                        control={form.control}
                        name={`disks.${index}.bus`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Bus</FormLabel>
                            <Select
                              value={field.value}
                              onValueChange={field.onChange}
                            >
                              <FormControl>
                                <SelectTrigger>
                                  <SelectValue />
                                </SelectTrigger>
                              </FormControl>
                              <SelectContent>
                                <SelectItem value='virtio-blk'>
                                  VirtIO block
                                </SelectItem>
                                <SelectItem value='virtio-scsi'>
                                  VirtIO SCSI
                                </SelectItem>
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
                  className='w-fit'
                  onClick={() =>
                    disks.append({
                      imageId: '',
                      format: 'qcow2',
                      bus: 'virtio-blk',
                    })
                  }
                >
                  <Plus className='size-4' /> Add Disk
                </Button>
                {managedDisks.isError && (
                  <Alert variant='destructive'>
                    <HardDrive className='size-4' />
                    <AlertTitle>Disks unavailable</AlertTitle>
                    <AlertDescription>
                      The managed disk inventory could not be loaded.
                      <Button
                        type='button'
                        variant='link'
                        className='ms-1 h-auto p-0'
                        onClick={() => managedDisks.refetch()}
                      >
                        Retry
                      </Button>
                    </AlertDescription>
                  </Alert>
                )}
                {!managedDisks.isPending &&
                  !managedDisks.isError &&
                  !managedDisks.data?.length && (
                    <Alert>
                      <HardDrive className='size-4' />
                      <AlertTitle>No managed disks</AlertTitle>
                      <AlertDescription>
                        Add a disk to the configured disk root or run an
                        automated Fedora install first.
                      </AlertDescription>
                    </Alert>
                  )}
              </div>
            </FormSection>

            <FormSection
              id='network'
              index='04'
              title='Network'
              description='Connect the primary VirtIO interface.'
            >
              {values.machineType === 'microvm' ? (
                <div className='border border-border bg-card p-4'>
                  <p className='text-sm font-medium'>User-mode NAT</p>
                  <p className='mt-1 text-sm text-muted-foreground'>
                    Outbound networking without a host bridge.
                  </p>
                </div>
              ) : (
                <FormField
                  control={form.control}
                  name='networkId'
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Libvirt network</FormLabel>
                      <Select
                        value={field.value}
                        onValueChange={field.onChange}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue
                              placeholder={
                                networks.isPending
                                  ? 'Loading networks…'
                                  : 'Select network'
                              }
                            />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {networks.data?.map((network) => (
                            <SelectItem
                              key={network.id}
                              value={network.id}
                              disabled={!network.active}
                            >
                              {network.id}
                              {network.bridge ? ` · ${network.bridge}` : ''}
                              {!network.active ? ' · inactive' : ''}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
            </FormSection>

            <FormSection
              id='access'
              index='05'
              title='Access'
              description='Configure browser console and an optional installation ISO.'
            >
              <div className='grid gap-5'>
                <FormField
                  control={form.control}
                  name='graphics'
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Graphics</FormLabel>
                      <Select
                        value={field.value}
                        onValueChange={field.onChange}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          <SelectItem value='vnc'>
                            Browser console (VNC)
                          </SelectItem>
                          <SelectItem value='none'>No graphics</SelectItem>
                        </SelectContent>
                      </Select>
                      <FormDescription>
                        VNC listens on loopback and uses one-time tickets.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <div className='grid gap-3'>
                  <div className='flex items-center justify-between gap-3'>
                    <div>
                      <p className='text-sm font-medium'>CD-ROM trays</p>
                      <p className='text-sm text-muted-foreground'>
                        Loaded media boots before disks.
                      </p>
                    </div>
                    <Button
                      type='button'
                      size='sm'
                      variant='outline'
                      onClick={() =>
                        cdroms.append({
                          id: `cdrom-${cdroms.fields.length + 1}`,
                          mediaId: null,
                        })
                      }
                    >
                      <Plus className='size-4' /> Add Tray
                    </Button>
                  </div>
                  {cdroms.fields.map((cdrom, index) => (
                    <div
                      key={cdrom.id}
                      className='grid gap-3 border border-border p-4 sm:grid-cols-[1fr_1.4fr_auto] sm:items-start'
                    >
                      <FormField
                        control={form.control}
                        name={`cdroms.${index}.id`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Tray ID</FormLabel>
                            <FormControl>
                              <Input {...field} />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                      <FormField
                        control={form.control}
                        name={`cdroms.${index}.mediaId`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>ISO</FormLabel>
                            <Select
                              value={field.value ?? 'none'}
                              onValueChange={(value) =>
                                field.onChange(value === 'none' ? null : value)
                              }
                            >
                              <FormControl>
                                <SelectTrigger>
                                  <SelectValue />
                                </SelectTrigger>
                              </FormControl>
                              <SelectContent>
                                <SelectItem value='none'>Empty</SelectItem>
                                {isos.data
                                  ?.filter(
                                    (resource) => resource.status === 'ready'
                                  )
                                  .map((resource) => (
                                    <SelectItem
                                      key={resource.id}
                                      value={resource.id}
                                    >
                                      {resource.id}
                                    </SelectItem>
                                  ))}
                              </SelectContent>
                            </Select>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                      <Button
                        type='button'
                        variant='ghost'
                        size='icon'
                        className='sm:mt-7'
                        aria-label={`Remove CD-ROM tray ${index + 1}`}
                        onClick={() => cdroms.remove(index)}
                      >
                        <Trash2 className='size-4' />
                      </Button>
                    </div>
                  ))}
                </div>
                <FormField
                  control={form.control}
                  name='serialLog'
                  render={({ field }) => (
                    <FormItem className='flex items-center justify-between gap-6 border border-border p-4'>
                      <div>
                        <FormLabel>Serial log</FormLabel>
                        <FormDescription>
                          Write the guest serial console to the managed log
                          root.
                        </FormDescription>
                      </div>
                      <FormControl>
                        <Switch
                          checked={field.value}
                          onCheckedChange={field.onChange}
                        />
                      </FormControl>
                    </FormItem>
                  )}
                />
              </div>
            </FormSection>

            {serverError && (
              <Alert variant='destructive' aria-live='polite'>
                <AlertTitle>Could not define VM</AlertTitle>
                <AlertDescription>{serverError}</AlertDescription>
              </Alert>
            )}
          </div>

          <aside>
            <div className='sticky top-20 border border-border bg-card'>
              <div className='border-b border-border px-4 py-3 font-mono text-[0.625rem] tracking-[0.14em] text-muted-foreground uppercase'>
                Review
              </div>
              <ReviewValue label='Name' value={values.name || 'Not set'} mono />
              <ReviewValue
                label='Machine'
                value={
                  values.machineType === 'microvm' ? 'MicroVM' : 'Standard VM'
                }
              />
              <ReviewValue
                label='Compute'
                value={`${values.vcpus ?? 2} CPU · ${formatMemory(Number(values.memoryMib ?? 4096))}`}
              />
              <ReviewValue
                label='Storage'
                value={
                  values.disks?.filter((disk) => disk?.imageId).length
                    ? `${values.disks.filter((disk) => disk?.imageId).length} disk(s)`
                    : 'Not set'
                }
              />
              <ReviewValue
                label='Network'
                value={
                  values.machineType === 'microvm'
                    ? 'User-mode NAT'
                    : values.networkId || 'Not set'
                }
                mono
              />
              <ReviewValue
                label='Graphics'
                value={values.graphics === 'vnc' ? 'Browser console' : 'None'}
              />
              <ReviewValue
                label='CD-ROMs'
                value={
                  values.cdroms?.length
                    ? `${values.cdroms.length} tray(s)`
                    : 'None'
                }
              />
              <div className='grid gap-2 border-t border-border p-4'>
                <Button
                  type='submit'
                  disabled={create.isPending || !managedDisks.data?.length}
                >
                  {create.isPending ? 'Defining…' : 'Define VM'}
                  {!create.isPending && <Check className='size-4' />}
                </Button>
                <Button type='button' variant='ghost' asChild>
                  <Link to='/vms'>Cancel</Link>
                </Button>
                <p className='pt-2 text-center text-xs text-muted-foreground'>
                  The VM will remain powered off.
                </p>
              </div>
            </div>
          </aside>
        </div>
      </form>
    </Form>
  )
}

function FormSection({
  id,
  index,
  title,
  description,
  children,
}: {
  id: string
  index: string
  title: string
  description: string
  children: React.ReactNode
}) {
  return (
    <section id={id} className='scroll-mt-20 border-t border-border pt-5'>
      <div className='mb-6 grid gap-2 sm:grid-cols-[3rem_1fr]'>
        <span className='font-mono text-[0.625rem] text-muted-foreground'>
          {index}
        </span>
        <div>
          <h2 className='text-lg font-medium tracking-tight'>{title}</h2>
          <p className='mt-1 text-sm text-muted-foreground'>{description}</p>
        </div>
      </div>
      <div className='sm:ps-12'>{children}</div>
    </section>
  )
}

function ReviewValue({
  label,
  value,
  mono = false,
}: {
  label: string
  value: string
  mono?: boolean
}) {
  return (
    <div className='grid grid-cols-[5rem_1fr] gap-3 border-b border-border px-4 py-3'>
      <span className='text-xs text-muted-foreground'>{label}</span>
      <span className={`truncate text-xs ${mono ? 'font-mono' : ''}`}>
        {value}
      </span>
    </div>
  )
}

function formatMemory(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '—'
  return value % 1024 === 0 ? `${value / 1024} GiB` : `${value} MiB`
}
