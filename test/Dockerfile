FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends openssh-server ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/run/sshd \
    && useradd -m -s /bin/bash resh \
    && mkdir -p /home/resh/.ssh \
    && chown -R resh:resh /home/resh/.ssh \
    && chmod 700 /home/resh/.ssh \
    && sed -i 's/^#*PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config

EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
