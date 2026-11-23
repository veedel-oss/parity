# Test target for the end-to-end tests: sshd, sudo, su, a user that may use
# sudo with a password. Containers share the host kernel, so these tests cover
# the user space of a release (ssh, sudo, su, dpkg), not its kernel.
ARG BASE=debian:trixie
FROM ${BASE}
RUN if grep -q 'VERSION_CODENAME=stretch' /etc/os-release; then \
      echo 'deb http://archive.debian.org/debian stretch main' > /etc/apt/sources.list; \
    fi \
 && apt-get -o Acquire::Check-Valid-Until=false update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends openssh-server sudo \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -s /bin/bash tester \
 && useradd -m -s /bin/bash nosudo \
 && echo 'tester:testerpw' | chpasswd \
 && echo 'nosudo:nosudopw' | chpasswd \
 && echo 'root:rootpw' | chpasswd \
 && adduser tester sudo \
 && mkdir -p /run/sshd \
 && for u in tester nosudo; do install -d -o $u -g $u -m 700 /home/$u/.ssh; done
COPY authorized_keys /home/tester/.ssh/authorized_keys
COPY authorized_keys /home/nosudo/.ssh/authorized_keys
COPY authorized_keys /root/.ssh/authorized_keys
RUN chown tester: /home/tester/.ssh/authorized_keys \
 && chown nosudo: /home/nosudo/.ssh/authorized_keys \
 && chmod 600 /home/*/.ssh/authorized_keys /root/.ssh/authorized_keys \
 && sed -i 's/^#\?PermitRootLogin.*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
