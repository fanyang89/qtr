import { useRef, useState, type DragEvent, type FormEvent } from 'react'
import axios from 'axios'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Disc3, Trash2, Upload, X } from 'lucide-react'
import { toast } from 'sonner'
import { deleteIso, uploadIso, type ManagedIso } from '@/lib/api'
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
import { ConfirmDialog } from '@/components/confirm-dialog'

const MAX_ISO_BYTES = 32 * 1024 * 1024 * 1024

export function IsoUploadButton() {
  const queryClient = useQueryClient()
  const controller = useRef<AbortController | null>(null)
  const [open, setOpen] = useState(false)
  const [file, setFile] = useState<File | null>(null)
  const [isoId, setIsoId] = useState('')
  const [loaded, setLoaded] = useState(0)
  const [uploading, setUploading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  function selectFile(selected: File | null) {
    setError(null)
    setLoaded(0)
    setFile(selected)
    if (selected) setIsoId(sanitizeIsoId(selected.name))
  }

  function close(force = false) {
    if (uploading && !force) return
    setOpen(false)
    selectFile(null)
    setIsoId('')
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!file) {
      setError('Select an ISO file.')
      return
    }
    if (!isValidIsoId(isoId)) {
      setError(
        'ISO ID must end in .iso and use letters, numbers, dot, underscore, or hyphen.'
      )
      return
    }
    if (file.size > MAX_ISO_BYTES) {
      setError('ISO exceeds the 32 GiB upload limit.')
      return
    }

    const abortController = new AbortController()
    controller.current = abortController
    setUploading(true)
    setError(null)
    try {
      await uploadIso(isoId, file, {
        signal: abortController.signal,
        onProgress: (value) => setLoaded(value),
      })
      await queryClient.invalidateQueries({ queryKey: ['resources', 'isos'] })
      toast.success(`${isoId} uploaded`)
      setUploading(false)
      close(true)
    } catch (uploadError) {
      if (axios.isCancel(uploadError)) {
        setError('Upload cancelled.')
      } else {
        const detail = axios.isAxiosError(uploadError)
          ? uploadError.response?.data?.detail
          : null
        setError(typeof detail === 'string' ? detail : 'ISO upload failed.')
      }
      setUploading(false)
    } finally {
      controller.current = null
    }
  }

  const progress = file?.size
    ? Math.min(100, Math.round((loaded / file.size) * 100))
    : 0
  const finalizing = uploading && file != null && loaded >= file.size

  return (
    <>
      <Button onClick={() => setOpen(true)}>
        <Upload className='size-4' />
        Upload ISO
      </Button>
      <Dialog
        open={open}
        onOpenChange={(next) => (next ? setOpen(true) : close())}
      >
        <DialogContent className='sm:max-w-xl'>
          <DialogHeader>
            <DialogTitle>Upload ISO</DialogTitle>
            <DialogDescription>
              Add installation media to the managed ISO root.
            </DialogDescription>
          </DialogHeader>
          <form className='grid gap-5' onSubmit={submit}>
            <label
              htmlFor='iso-file'
              className='grid min-h-40 cursor-pointer place-items-center border border-dashed border-border bg-muted/20 p-6 text-center transition-colors hover:bg-muted/40'
              onDragOver={(event) => event.preventDefault()}
              onDrop={(event: DragEvent<HTMLLabelElement>) => {
                event.preventDefault()
                selectFile(event.dataTransfer.files.item(0))
              }}
            >
              <span>
                <Disc3 className='mx-auto mb-3 size-6 text-muted-foreground' />
                <span className='block text-sm font-medium'>
                  Drop an ISO here
                </span>
                <span className='mt-1 block text-xs text-muted-foreground'>
                  or select a file · 32 GiB maximum
                </span>
              </span>
            </label>
            <input
              id='iso-file'
              type='file'
              accept='.iso,application/x-iso9660-image'
              className='sr-only'
              disabled={uploading}
              onChange={(event) =>
                selectFile(event.target.files?.item(0) ?? null)
              }
            />

            {file && (
              <div className='grid gap-4 border border-border bg-card p-4'>
                <div className='flex items-center justify-between gap-4'>
                  <div className='min-w-0'>
                    <p className='truncate font-mono text-xs'>{file.name}</p>
                    <p className='mt-1 text-xs text-muted-foreground'>
                      {formatBytes(file.size)}
                    </p>
                  </div>
                  {!uploading && (
                    <Button
                      type='button'
                      variant='ghost'
                      size='icon'
                      aria-label='Clear selected ISO'
                      onClick={() => selectFile(null)}
                    >
                      <X className='size-4' />
                    </Button>
                  )}
                </div>
                <div className='grid gap-2'>
                  <Label htmlFor='iso-id'>ISO ID</Label>
                  <Input
                    id='iso-id'
                    value={isoId}
                    disabled={uploading}
                    className='font-mono'
                    onChange={(event) => setIsoId(event.target.value)}
                  />
                </div>
              </div>
            )}

            {uploading && (
              <div className='grid gap-2' aria-live='polite'>
                <div className='flex justify-between font-mono text-[0.6875rem] text-muted-foreground uppercase'>
                  <span>{finalizing ? 'Finalizing' : 'Uploading'}</span>
                  <span>{progress}%</span>
                </div>
                <div
                  className='h-1.5 overflow-hidden bg-muted'
                  role='progressbar'
                  aria-valuenow={progress}
                >
                  <div
                    className='h-full bg-primary transition-[width]'
                    style={{ width: `${progress}%` }}
                  />
                </div>
              </div>
            )}
            {error && (
              <p className='text-sm text-destructive' aria-live='polite'>
                {error}
              </p>
            )}

            <DialogFooter>
              <Button
                type='button'
                variant='ghost'
                onClick={() =>
                  uploading ? controller.current?.abort() : close()
                }
              >
                {uploading ? 'Cancel Upload' : 'Cancel'}
              </Button>
              <Button type='submit' disabled={!file || uploading}>
                {uploading ? 'Uploading…' : 'Upload ISO'}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}

export function DeleteIsoButton({ iso }: { iso: ManagedIso }) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  let blocker: string | null = null
  if (iso.reservedByJobIds.length) {
    blocker = 'Reserved by an install job'
  } else if (iso.attachments.length) {
    blocker = `Attached to ${iso.attachments[0].vmName}`
  }
  const remove = useMutation({
    mutationFn: () => deleteIso(iso.id),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['resources', 'isos'] })
      toast.success(`${iso.id} deleted`)
      setOpen(false)
    },
    onError: (removeError) => {
      const detail = axios.isAxiosError(removeError)
        ? removeError.response?.data?.detail
        : null
      setError(typeof detail === 'string' ? detail : 'Delete failed.')
    },
  })

  return (
    <>
      <Button
        variant='ghost'
        size='icon'
        aria-label={`Delete ${iso.id}`}
        disabled={blocker != null}
        title={blocker ?? `Delete ${iso.id}`}
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
        title={`Delete ${iso.id}?`}
        desc='This permanently removes the ISO from managed storage.'
        confirmText='Delete'
        destructive
        isLoading={remove.isPending}
        handleConfirm={() => remove.mutate()}
      >
        {error && (
          <p role='alert' className='text-sm text-destructive'>
            {error}
          </p>
        )}
      </ConfirmDialog>
    </>
  )
}

function sanitizeIsoId(name: string): string {
  return name.replace(/[^a-zA-Z0-9._-]+/g, '-').replace(/^\.+/, '')
}

function isValidIsoId(value: string): boolean {
  return (
    value.length > 4 &&
    value.length <= 255 &&
    /^[a-zA-Z0-9][a-zA-Z0-9._-]*\.iso$/i.test(value)
  )
}
