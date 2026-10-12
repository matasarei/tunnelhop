; What TunnelHop's part of the installer says, in the installer's languages. Included from
; hooks.nsh as UTF-8.
;
; The installer picks its language from Windows; $LANGUAGE holds it as a Windows language id.
; The texts are chosen by that id when a message is shown, and not as NSIS LangStrings: hooks
; are read before the installer's languages are loaded, where a LangString cannot name one.
;
; $0 in a text is the exit code of the command that failed.

!define TH_STOPPING_MAYBE_EN "Stopping the TunnelHop service, if it is running..."
!define TH_INSTALLING_EN "Installing the TunnelHop service..."
!define TH_INSTALL_FAILED_EN "The TunnelHop service could not be installed (code $0).$\n$\nThe app will say the service is not running. Run this installer again, or, from an administrator prompt:"
!define TH_STOPPING_EN "Stopping the TunnelHop service..."
!define TH_REMOVING_EN "Removing the TunnelHop service and its data..."
!define TH_REMOVE_FAILED_EN "The TunnelHop service could not be removed completely (code $0).$\n$\nIf the internet is blocked afterwards, run from an administrator prompt, before deleting the file:"
!define TH_SECURING_EN "Restricting the install folder to administrators..."
!define TH_SECURE_FAILED_EN "The install folder's permissions could not be restricted to administrators (code $0), so nothing was installed: the TunnelHop service runs from this folder with full rights over the PC.$\n$\nChoose another folder, for example under Program Files, and run this installer again."

!define TH_STOPPING_MAYBE_UK "Зупинка служби TunnelHop, якщо її запущено..."
!define TH_INSTALLING_UK "Встановлення служби TunnelHop..."
!define TH_INSTALL_FAILED_UK "Не вдалося встановити службу TunnelHop (код $0).$\n$\nЗастосунок повідомлятиме, що службу не запущено. Запустіть цей інсталятор ще раз або виконайте в командному рядку від імені адміністратора:"
!define TH_STOPPING_UK "Зупинка служби TunnelHop..."
!define TH_REMOVING_UK "Видалення служби TunnelHop та її даних..."
!define TH_REMOVE_FAILED_UK "Не вдалося повністю видалити службу TunnelHop (код $0).$\n$\nЯкщо після цього інтернет буде заблоковано, виконайте в командному рядку від імені адміністратора, перш ніж видаляти файл:"
!define TH_SECURING_UK "Обмеження доступу до папки встановлення адміністраторами..."
!define TH_SECURE_FAILED_UK "Не вдалося обмежити доступ до папки встановлення лише адміністраторами (код $0), тому нічого не встановлено: служба TunnelHop працює з цієї папки з повними правами на комп’ютері.$\n$\nВиберіть іншу папку, наприклад у Program Files, і запустіть цей інсталятор ще раз."

!define TH_STOPPING_MAYBE_RU "Остановка службы TunnelHop, если она запущена..."
!define TH_INSTALLING_RU "Установка службы TunnelHop..."
!define TH_INSTALL_FAILED_RU "Не удалось установить службу TunnelHop (код $0).$\n$\nПриложение будет сообщать, что служба не запущена. Запустите этот установщик ещё раз или выполните в командной строке от имени администратора:"
!define TH_STOPPING_RU "Остановка службы TunnelHop..."
!define TH_REMOVING_RU "Удаление службы TunnelHop и её данных..."
!define TH_REMOVE_FAILED_RU "Не удалось полностью удалить службу TunnelHop (код $0).$\n$\nЕсли после этого интернет будет заблокирован, выполните в командной строке от имени администратора, прежде чем удалять файл:"
!define TH_SECURING_RU "Ограничение доступа к папке установки администраторами..."
!define TH_SECURE_FAILED_RU "Не удалось ограничить доступ к папке установки только администраторами (код $0), поэтому ничего не установлено: служба TunnelHop работает из этой папки с полными правами на компьютере.$\n$\nВыберите другую папку, например в Program Files, и запустите этот установщик ещё раз."

!define TH_STOPPING_MAYBE_ES "Deteniendo el servicio de TunnelHop, si está en ejecución..."
!define TH_INSTALLING_ES "Instalando el servicio de TunnelHop..."
!define TH_INSTALL_FAILED_ES "No se pudo instalar el servicio de TunnelHop (código $0).$\n$\nLa aplicación indicará que el servicio no está en ejecución. Ejecute de nuevo este instalador o, desde un símbolo del sistema de administrador:"
!define TH_STOPPING_ES "Deteniendo el servicio de TunnelHop..."
!define TH_REMOVING_ES "Quitando el servicio de TunnelHop y sus datos..."
!define TH_REMOVE_FAILED_ES "No se pudo quitar por completo el servicio de TunnelHop (código $0).$\n$\nSi después Internet queda bloqueado, ejecute desde un símbolo del sistema de administrador, antes de eliminar el archivo:"
!define TH_SECURING_ES "Restringiendo la carpeta de instalación a los administradores..."
!define TH_SECURE_FAILED_ES "No se pudieron restringir los permisos de la carpeta de instalación a los administradores (código $0), así que no se instaló nada: el servicio de TunnelHop se ejecuta desde esta carpeta con todos los permisos sobre el equipo.$\n$\nElija otra carpeta, por ejemplo dentro de Program Files, y ejecute de nuevo este instalador."

!define TH_STOPPING_MAYBE_FR "Arrêt du service TunnelHop, s’il est démarré..."
!define TH_INSTALLING_FR "Installation du service TunnelHop..."
!define TH_INSTALL_FAILED_FR "Le service TunnelHop n’a pas pu être installé (code $0).$\n$\nL’application indiquera que le service n’est pas démarré. Relancez ce programme d’installation ou, depuis une invite de commandes administrateur :"
!define TH_STOPPING_FR "Arrêt du service TunnelHop..."
!define TH_REMOVING_FR "Suppression du service TunnelHop et de ses données..."
!define TH_REMOVE_FAILED_FR "Le service TunnelHop n’a pas pu être entièrement supprimé (code $0).$\n$\nSi Internet est ensuite bloqué, exécutez depuis une invite de commandes administrateur, avant de supprimer le fichier :"
!define TH_SECURING_FR "Restriction du dossier d’installation aux administrateurs..."
!define TH_SECURE_FAILED_FR "Les autorisations du dossier d’installation n’ont pas pu être restreintes aux administrateurs (code $0) ; rien n’a donc été installé : le service TunnelHop s’exécute depuis ce dossier avec tous les droits sur le PC.$\n$\nChoisissez un autre dossier, par exemple dans Program Files, et relancez ce programme d’installation."

!define TH_STOPPING_MAYBE_DE "Der TunnelHop-Dienst wird beendet, falls er läuft..."
!define TH_INSTALLING_DE "Der TunnelHop-Dienst wird installiert..."
!define TH_INSTALL_FAILED_DE "Der TunnelHop-Dienst konnte nicht installiert werden (Code $0).$\n$\nDie App wird melden, dass der Dienst nicht läuft. Führen Sie dieses Installationsprogramm erneut aus oder, in einer Administrator-Eingabeaufforderung:"
!define TH_STOPPING_DE "Der TunnelHop-Dienst wird beendet..."
!define TH_REMOVING_DE "Der TunnelHop-Dienst und seine Daten werden entfernt..."
!define TH_REMOVE_FAILED_DE "Der TunnelHop-Dienst konnte nicht vollständig entfernt werden (Code $0).$\n$\nFalls das Internet danach blockiert ist, führen Sie in einer Administrator-Eingabeaufforderung Folgendes aus, bevor Sie die Datei löschen:"
!define TH_SECURING_DE "Der Installationsordner wird auf Administratoren beschränkt..."
!define TH_SECURE_FAILED_DE "Die Berechtigungen des Installationsordners konnten nicht auf Administratoren beschränkt werden (Code $0), daher wurde nichts installiert: Der TunnelHop-Dienst läuft aus diesem Ordner mit vollen Rechten auf dem PC.$\n$\nWählen Sie einen anderen Ordner, zum Beispiel unter Program Files, und führen Sie dieses Installationsprogramm erneut aus."

!define TH_STOPPING_MAYBE_PT "Parando o serviço do TunnelHop, se estiver em execução..."
!define TH_INSTALLING_PT "Instalando o serviço do TunnelHop..."
!define TH_INSTALL_FAILED_PT "Não foi possível instalar o serviço do TunnelHop (código $0).$\n$\nO aplicativo dirá que o serviço não está em execução. Execute este instalador novamente ou, em um prompt de comando de administrador:"
!define TH_STOPPING_PT "Parando o serviço do TunnelHop..."
!define TH_REMOVING_PT "Removendo o serviço do TunnelHop e seus dados..."
!define TH_REMOVE_FAILED_PT "Não foi possível remover completamente o serviço do TunnelHop (código $0).$\n$\nSe a internet ficar bloqueada depois disso, execute em um prompt de comando de administrador, antes de excluir o arquivo:"
!define TH_SECURING_PT "Restringindo a pasta de instalação aos administradores..."
!define TH_SECURE_FAILED_PT "Não foi possível restringir as permissões da pasta de instalação aos administradores (código $0), então nada foi instalado: o serviço do TunnelHop é executado a partir desta pasta com todos os direitos sobre o PC.$\n$\nEscolha outra pasta, por exemplo em Program Files, e execute este instalador novamente."

!define TH_STOPPING_MAYBE_IT "Arresto del servizio TunnelHop, se è in esecuzione..."
!define TH_INSTALLING_IT "Installazione del servizio TunnelHop..."
!define TH_INSTALL_FAILED_IT "Impossibile installare il servizio TunnelHop (codice $0).$\n$\nL’app indicherà che il servizio non è in esecuzione. Esegui di nuovo questo programma di installazione oppure, da un prompt dei comandi come amministratore:"
!define TH_STOPPING_IT "Arresto del servizio TunnelHop..."
!define TH_REMOVING_IT "Rimozione del servizio TunnelHop e dei suoi dati..."
!define TH_REMOVE_FAILED_IT "Impossibile rimuovere completamente il servizio TunnelHop (codice $0).$\n$\nSe in seguito Internet risulta bloccato, esegui da un prompt dei comandi come amministratore, prima di eliminare il file:"
!define TH_SECURING_IT "Limitazione della cartella di installazione agli amministratori..."
!define TH_SECURE_FAILED_IT "Impossibile limitare agli amministratori le autorizzazioni della cartella di installazione (codice $0), quindi non è stato installato nulla: il servizio TunnelHop viene eseguito da questa cartella con pieni diritti sul PC.$\n$\nScegli un’altra cartella, ad esempio in Program Files, ed esegui di nuovo questo programma di installazione."

!define TH_STOPPING_MAYBE_PL "Zatrzymywanie usługi TunnelHop, jeśli działa..."
!define TH_INSTALLING_PL "Instalowanie usługi TunnelHop..."
!define TH_INSTALL_FAILED_PL "Nie udało się zainstalować usługi TunnelHop (kod $0).$\n$\nAplikacja będzie informować, że usługa nie działa. Uruchom ten instalator ponownie albo wykonaj w wierszu polecenia administratora:"
!define TH_STOPPING_PL "Zatrzymywanie usługi TunnelHop..."
!define TH_REMOVING_PL "Usuwanie usługi TunnelHop i jej danych..."
!define TH_REMOVE_FAILED_PL "Nie udało się całkowicie usunąć usługi TunnelHop (kod $0).$\n$\nJeśli potem internet będzie zablokowany, wykonaj w wierszu polecenia administratora, zanim usuniesz plik:"
!define TH_SECURING_PL "Ograniczanie folderu instalacji do administratorów..."
!define TH_SECURE_FAILED_PL "Nie udało się ograniczyć uprawnień folderu instalacji do administratorów (kod $0), więc nic nie zostało zainstalowane: usługa TunnelHop działa z tego folderu z pełnymi uprawnieniami do komputera.$\n$\nWybierz inny folder, na przykład w Program Files, i uruchom ten instalator ponownie."

!define TH_STOPPING_MAYBE_ZH "正在停止 TunnelHop 服务（如果它正在运行）..."
!define TH_INSTALLING_ZH "正在安装 TunnelHop 服务..."
!define TH_INSTALL_FAILED_ZH "无法安装 TunnelHop 服务（代码 $0）。$\n$\n应用会提示服务未运行。请重新运行此安装程序，或在管理员命令提示符中运行："
!define TH_STOPPING_ZH "正在停止 TunnelHop 服务..."
!define TH_REMOVING_ZH "正在移除 TunnelHop 服务及其数据..."
!define TH_REMOVE_FAILED_ZH "无法完全移除 TunnelHop 服务（代码 $0）。$\n$\n如果之后网络被阻止，请在删除该文件之前，在管理员命令提示符中运行："
!define TH_SECURING_ZH "正在将安装文件夹限制为仅管理员可修改..."
!define TH_SECURE_FAILED_ZH "无法将安装文件夹的权限限制为仅管理员（代码 $0），因此未安装任何内容：TunnelHop 服务从此文件夹运行，对这台电脑拥有完全权限。$\n$\n请选择其他文件夹（例如 Program Files 下的文件夹），然后重新运行此安装程序。"

!define TH_STOPPING_MAYBE_JA "TunnelHop サービスが実行中の場合は停止しています..."
!define TH_INSTALLING_JA "TunnelHop サービスをインストールしています..."
!define TH_INSTALL_FAILED_JA "TunnelHop サービスをインストールできませんでした (コード $0)。$\n$\nアプリにはサービスが実行されていないと表示されます。このインストーラーをもう一度実行するか、管理者のコマンド プロンプトで次を実行してください:"
!define TH_STOPPING_JA "TunnelHop サービスを停止しています..."
!define TH_REMOVING_JA "TunnelHop サービスとそのデータを削除しています..."
!define TH_REMOVE_FAILED_JA "TunnelHop サービスを完全には削除できませんでした (コード $0)。$\n$\nその後インターネットがブロックされた場合は、ファイルを削除する前に、管理者のコマンド プロンプトで次を実行してください:"
!define TH_SECURING_JA "インストール フォルダーを管理者のみに制限しています..."
!define TH_SECURE_FAILED_JA "インストール フォルダーのアクセス許可を管理者のみに制限できませんでした (コード $0)。そのため、何もインストールされていません。TunnelHop サービスはこのフォルダーから PC に対する完全な権限で実行されます。$\n$\n別のフォルダー (Program Files の下など) を選び、このインストーラーをもう一度実行してください。"

; $R9 := the text named KEY in the installer's language, English when it has no other.
!macro TH_TEXT KEY
  ${Switch} $LANGUAGE
    ${Case} 1058
      StrCpy $R9 "${TH_${KEY}_UK}"
      ${Break}
    ${Case} 1049
      StrCpy $R9 "${TH_${KEY}_RU}"
      ${Break}
    ${Case} 1034
      StrCpy $R9 "${TH_${KEY}_ES}"
      ${Break}
    ${Case} 1036
      StrCpy $R9 "${TH_${KEY}_FR}"
      ${Break}
    ${Case} 1031
      StrCpy $R9 "${TH_${KEY}_DE}"
      ${Break}
    ${Case} 1046
      StrCpy $R9 "${TH_${KEY}_PT}"
      ${Break}
    ${Case} 1040
      StrCpy $R9 "${TH_${KEY}_IT}"
      ${Break}
    ${Case} 1045
      StrCpy $R9 "${TH_${KEY}_PL}"
      ${Break}
    ${Case} 2052
      StrCpy $R9 "${TH_${KEY}_ZH}"
      ${Break}
    ${Case} 1041
      StrCpy $R9 "${TH_${KEY}_JA}"
      ${Break}
    ${Default}
      StrCpy $R9 "${TH_${KEY}_EN}"
      ${Break}
  ${EndSwitch}
!macroend

; Print the text named KEY in the installer's log.
!macro TH_PRINT KEY
  Push $R9
  !insertmacro TH_TEXT ${KEY}
  DetailPrint "$R9"
  Pop $R9
!macroend

; Show the text named KEY as a warning, with the command to run under it. /SD keeps a silent
; install (/S) from ever waiting on the box.
!macro TH_WARN KEY COMMAND
  Push $R9
  !insertmacro TH_TEXT ${KEY}
  MessageBox MB_ICONEXCLAMATION|MB_OK "$R9$\n$\n$\"$INSTDIR\th-service.exe$\" ${COMMAND}" /SD IDOK
  Pop $R9
!macroend

; Show the text named KEY as an error the install cannot go past. The caller aborts after it.
!macro TH_ERROR KEY
  Push $R9
  !insertmacro TH_TEXT ${KEY}
  MessageBox MB_ICONSTOP|MB_OK "$R9" /SD IDOK
  Pop $R9
!macroend
