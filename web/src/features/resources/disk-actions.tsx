import { useState, type FormEvent } from 'react'
import axios from 'axios'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { HardDrive, Plus } from 'lucide-react'
import { toast } from 'sonner'
import { createDisk, type ImageCreateInput } from '@/lib/api'
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
      close()
    },
    onError: (createError) => {
      const detail = axios.isAxiosError(createError)
        ? createError.response?.data?.detail
        : null
      setError(typeof detail === 'string' ? detail : 'Disk creation failed.')
    },
  })

  function close() {
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
              <Button type='button' variant='ghost' onClick={close}>
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
