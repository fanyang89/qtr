import { z } from 'zod'
import axios from 'axios'
import { toast } from 'sonner'

const serverErrorSchema = z.object({
  title: z.string().min(1).optional(),
  detail: z.string().min(1).optional(),
  error: z.string().min(1).optional(),
  message: z.string().min(1).optional(),
})

export function handleServerError(error: unknown) {
  if (import.meta.env.DEV) {
    // eslint-disable-next-line no-console
    console.log(error)
  }

  let errMsg = 'Something went wrong!'

  if (
    error &&
    typeof error === 'object' &&
    'status' in error &&
    Number(error.status) === 204
  ) {
    errMsg = 'No content.'
  }

  if (axios.isAxiosError(error)) {
    const data = error.response?.data
    if (typeof data === 'string' && data.length > 0) {
      errMsg = data
    } else {
      const parsed = serverErrorSchema.safeParse(data)
      const message = parsed.success
        ? (parsed.data.detail ??
          parsed.data.title ??
          parsed.data.error ??
          parsed.data.message)
        : undefined
      if (message) {
        errMsg = message
      }
    }
  }

  toast.error(errMsg)
}
