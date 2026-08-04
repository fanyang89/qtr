FROM fedora:44

RUN dnf install -y \
        acl \
        cargo \
        libvirt-client \
        libvirt-daemon \
        libvirt-daemon-kvm \
        libvirt-devel \
        pkgconf-pkg-config \
        qemu-img \
        qemu-kvm \
        rust \
    && dnf clean all

WORKDIR /workspace

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
RUN cargo build --locked --release

COPY e2e/libvirt/run.sh e2e/libvirt/vm.yaml /opt/qtr-e2e/

ENTRYPOINT ["/opt/qtr-e2e/run.sh"]
