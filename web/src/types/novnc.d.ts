declare module '@novnc/novnc' {
  type RfbCredentials = {
    username?: string
    password?: string
    target?: string
  }

  type RfbOptions = {
    shared?: boolean
    credentials?: RfbCredentials
    wsProtocols?: string[]
    repeaterID?: string
  }

  export default class RFB extends EventTarget {
    viewOnly: boolean
    scaleViewport: boolean
    resizeSession: boolean
    focusOnClick: boolean
    qualityLevel: number
    compressionLevel: number

    constructor(target: HTMLElement, url: string, options?: RfbOptions)
    disconnect(): void
    sendCredentials(credentials: RfbCredentials): void
    sendCtrlAltDel(): void
    clipboardPasteFrom(text: string): void
  }
}
