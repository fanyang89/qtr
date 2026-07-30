import { useState, type FormEvent } from 'react'
import axios from 'axios'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { HardDrive, Maximize2, Plus, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import {
  createDisk,
  deleteDisk,
  resizeDisk,
  type ImageCreateInput,
  type ManagedImage,
} from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ConfirmDialog } from '@/components/confirm-dialog'

const GIB = 1024 ** 3

export function CreateDiskButton() {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [id, setId] = useState('')
  const [sizeGib, setSizeGib] = useState('40')
  const [format, setFormat] = useState<ImageCreateInput['format']>('qcow2')
  const [error, setError] = useState<string | null>(null)
  const create = useMutation({
    mutationFn: (input: ImageCreateInput) => createDisk(input),
    onSuccess: async (disk) => {
      await queryClient.invalidateQueries({ queryKey: ['resources', 'disks'] })
      toast.success(`${disk.id} created`)
      close(true)
    },
    onError: (createError) => {
      const detail = axios.isAxiosError(createError)
        ? createError.response?.data?.detail
        : null
      setError(typeof detail === 'string' ? detail : 'Disk creation failed.')
    },
  })

  function close(force = false) {
    if (create.isPending && !force) return
    setOpen(false)
    setId('')
    setSizeGib('40')
    setFormat('qcow2')
    setError(null)
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const size = Number(sizeGib)
    const extension = format === 'qcow2' ? '.qcow2' : '.raw'
    if (
      !new RegExp(`^[a-zA-Z0-9][a-zA-Z0-9._-]*\\${extension}$`, 'i').test(id)
    ) {
      setError(`Disk ID must end in ${extension}.`)
      return
    }
    if (!Number.isInteger(size) || size < 1 || size > 16384) {
      setError('Disk size must be 1–16384 GiB.')
      return
    }
    setError(null)
    create.mutate({ id, format, sizeBytes: size * GIB })
  }

  function changeFormat(value: ImageCreateInput['format']) {
    setFormat(value)
    setId((current) =>
      current.replace(
        /\.(?:qcow2|raw)$/i,
        value === 'qcow2' ? '.qcow2' : '.raw'
      )
    )
  }

  return (
    <>
      <Button onClick={() => setOpen(true)}>
        <Plus className='size-4' />
        Create Disk
      </Button>
      <Dialog
        open={open}
        onOpenChange={(next) => (next ? setOpen(true) : close())}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create disk</DialogTitle>
            <DialogDescription>
              Add a blank managed virtual disk.
            </DialogDescription>
          </DialogHeader>
          <form className='grid gap-5' onSubmit={submit}>
            <div className='grid gap-4 border border-border bg-card p-4'>
              <HardDrive className='size-5 text-muted-foreground' />
              <div className='grid gap-2'>
                <Label htmlFor='disk-id'>Disk ID</Label>
                <Input
                  id='disk-id'
                  value={id}
                  placeholder={`data${format === 'qcow2' ? '.qcow2' : '.raw'}`}
                  className='font-mono'
                  disabled={create.isPending}
                  onChange={(event) => setId(event.target.value)}
                />
              </div>
              <div className='grid gap-4 sm:grid-cols-2'>
                <div className='grid gap-2'>
                  <Label htmlFor='disk-size'>Size GiB</Label>
                  <Input
                    id='disk-size'
                    type='number'
                    min={1}
                    max={16384}
                    value={sizeGib}
                    disabled={create.isPending}
                    onChange={(event) => setSizeGib(event.target.value)}
                  />
                </div>
                <div className='grid gap-2'>
                  <Label htmlFor='disk-format'>Format</Label>
                  <Select
                    value={format}
                    disabled={create.isPending}
                    onValueChange={(value) =>
                      changeFormat(value as ImageCreateInput['format'])
                    }
                  >
                    <SelectTrigger id='disk-format' className='w-full'>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value='qcow2'>qcow2</SelectItem>
                      <SelectItem value='raw'>raw</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>
            </div>
            {error && (
              <p className='text-sm text-destructive' aria-live='polite'>
                {error}
              </p>
            )}
            <DialogFooter>
              <Button
                type='button'
                variant='ghost'
                disabled={create.isPending}
                onClick={() => close()}
              >
                Cancel
              </Button>
              <Button type='submit' disabled={create.isPending}>
                {create.isPending ? 'Creating…' : 'Create Disk'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}

export function ResizeDiskButton({ disk }: { disk: ManagedImage }) {
  const queryClient = useQueryClient()
  const currentBytes = disk.virtualSizeBytes ?? disk.sizeBytes
  const [open, setOpen] = useState(false)
  const [sizeGib, setSizeGib] = useState(String(Math.ceil(currentBytes / GIB)))
  const [error, setError] = useState<string | null>(null)
  const resize = useMutation({
    mutationFn: (sizeBytes: number) => resizeDisk(disk.id, sizeBytes),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['resources', 'disks'] })
      toast.success(`${disk.id} expanded`)
      setOpen(false)
    },
    onError: (resizeError) =>
      setError(errorDetail(resizeError, 'Disk expansion failed.')),
  })

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const size = Number(sizeGib)
    if (!Number.isInteger(size) || size < 1 || size > 16384) {
      setError('Disk size must be 1–16384 GiB.')
      return
    }
    const sizeBytes = size * GIB
    if (sizeBytes < currentBytes) {
      setError(`New capacity must be at least ${formatBytes(currentBytes)}.`)
      return
    }
    setError(null)
    resize.mutate(sizeBytes)
  }

  const unavailable =
    disk.status !== 'ready' ||
    disk.attachments.some((attachment) => attachment.active) ||
    Boolean(disk.reservedByJobId)

  return (
    <>
      <Button
        variant='ghost'
        size='icon'
        aria-label={`Expand ${disk.id}`}
        title={
          unavailable ? 'Stop the attached VM before expanding' : 'Expand disk'
        }
        disabled={unavailable}
        onClick={() => setOpen(true)}
      >
        <Maximize2 className='size-4 text-muted-foreground' />
      </Button>
      <Dialog
        open={open}
        onOpenChange={(next) => !resize.isPending && setOpen(next)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Expand {disk.id}</DialogTitle>
            <DialogDescription>
              Increase virtual capacity. Guest partitions are unchanged.
            </DialogDescription>
          </DialogHeader>
          <form className='grid gap-5' onSubmit={submit}>
            <div className='grid gap-2'>
              <Label htmlFor={`resize-${disk.id}`}>New size GiB</Label>
              <Input
                id={`resize-${disk.id}`}
                type='number'
                min={Math.ceil(currentBytes / GIB)}
                max={16384}
                value={sizeGib}
                disabled={resize.isPending}
                onChange={(event) => setSizeGib(event.target.value)}
              />
            </div>
            {error && <p className='text-sm text-destructive'>{error}</p>}
            <DialogFooter>
              <Button
                type='button'
                variant='ghost'
                disabled={resize.isPending}
                onClick={() => setOpen(false)}
              >
                Cancel
              </Button>
              <Button type='submit' disabled={resize.isPending}>
                {resize.isPending ? 'Expanding…' : 'Expand Disk'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}

export function DeleteDiskButton({ disk }: { disk: ManagedImage }) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const remove = useMutation({
    mutationFn: () => deleteDisk(disk.id),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['resources', 'disks'] })
      toast.success(`${disk.id} deleted`)
      setOpen(false)
    },
    onError: (deleteError) =>
      setError(errorDetail(deleteError, 'Disk deletion failed.')),
  })
  const unavailable =
    disk.attachments.length > 0 || Boolean(disk.reservedByJobId)

  return (
    <>
      <Button
        variant='ghost'
        size='icon'
        aria-label={`Delete ${disk.id}`}
        title={
          unavailable ? 'Detach the disk before deleting it' : 'Delete disk'
        }
        disabled={unavailable}
        onClick={() => {
          setError(null)
          setOpen(true)
        }}
      >
        <Trash2 className='size-4 text-muted-foreground' />
      </Button>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title={`Delete ${disk.id}?`}
        desc='This permanently removes the disk and all data stored on it.'
        confirmText='Delete'
        destructive
        isLoading={remove.isPending}
        handleConfirm={() => remove.mutate()}
      >
        {error && <p className='text-sm text-destructive'>{error}</p>}
      </ConfirmDialog>
    </>
  )
}

function errorDetail(error: unknown, fallback: string): string {
  const detail = axios.isAxiosError(error) ? error.response?.data?.detail : null
  return typeof detail === 'string' ? detail : fallback
}
