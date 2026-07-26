export function PageHeading({
  eyebrow,
  title,
  description,
  actions,
}: {
  eyebrow: string
  title: string
  description: string
  actions?: React.ReactNode
}) {
  return (
    <div className='mb-8 flex flex-col gap-5 border-b border-border pb-7 sm:flex-row sm:items-end sm:justify-between'>
      <div>
        <p className='mb-3 font-mono text-[0.625rem] tracking-[0.17em] text-muted-foreground uppercase'>
          {eyebrow}
        </p>
        <h1 className='text-3xl font-medium tracking-[-0.04em] sm:text-4xl'>
          {title}
        </h1>
        <p className='mt-2 max-w-xl text-sm text-muted-foreground'>
          {description}
        </p>
      </div>
      {actions && <div className='flex items-center gap-2'>{actions}</div>}
    </div>
  )
}
