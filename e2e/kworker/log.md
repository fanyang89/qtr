# kworker test logs

## 复现

### Try 1

virtio-blk 单队列 + cache=none + io=native

```yml

```

[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34917888 op 0x1:(WRITE) flags 0x8800 phys_seg 54 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34918912 op 0x1:(WRITE) flags 0x8800 phys_seg 113 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34920960 op 0x1:(WRITE) flags 0x8800 phys_seg 46 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34921984 op 0x1:(WRITE) flags 0x8800 phys_seg 52 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34923008 op 0x1:(WRITE) flags 0x8800 phys_seg 32 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34924032 op 0x1:(WRITE) flags 0x8800 phys_seg 48 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34925056 op 0x1:(WRITE) flags 0x8800 phys_seg 47 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34926080 op 0x1:(WRITE) flags 0x8800 phys_seg 41 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34927104 op 0x1:(WRITE) flags 0x8800 phys_seg 43 prio class 2
[Sat Jul  4 16:05:54 2026] critical space allocation error, dev loop0, sector 34928128 op 0x1:(WRITE) flags 0x8800 phys_seg 66 prio class 2
