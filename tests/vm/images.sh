# Operating systems for tests/vm/run.sh and their cloud images: the latest
# genericcloud image of each release from cloud.debian.org.
oses=(debian-10 debian-11 debian-12 debian-13)

image_url() { # os
    local codename
    case $1 in
        debian-10) codename=buster ;;
        debian-11) codename=bullseye ;;
        debian-12) codename=bookworm ;;
        debian-13) codename=trixie ;;
        *) return 1 ;;
    esac
    echo "https://cloud.debian.org/images/cloud/$codename/latest/debian-${1#debian-}-genericcloud-amd64.qcow2"
}
