# bash completion for termcode: its options, else files and folders.
_termcode() {
    local cur=${COMP_WORDS[COMP_CWORD]}
    if [[ $cur == -* ]]; then
        COMPREPLY=($(compgen -W '--tab --default-config --version --help -V -h' -- "$cur"))
    fi
}
complete -o default -F _termcode termcode
