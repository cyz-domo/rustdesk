import 'dart:async';
import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_hbb/common/widgets/setting_widgets.dart';
import 'package:flutter_hbb/common/widgets/toolbar.dart';
import 'package:get/get.dart';

import '../../common.dart';
import '../../models/platform_model.dart';

void _showSuccess() {
  showToast(translate("Successful"));
}

void setTemporaryPasswordLengthDialog(
    OverlayDialogManager dialogManager) async {
  List<String> lengths = ['6', '8', '10'];
  String length = await bind.mainGetOption(key: "temporary-password-length");
  var index = lengths.indexOf(length);
  if (index < 0) index = 0;
  length = lengths[index];
  dialogManager.show((setState, close, context) {
    setLength(newValue) {
      final oldValue = length;
      if (oldValue == newValue) return;
      setState(() {
        length = newValue;
      });
      bind.mainSetOption(key: "temporary-password-length", value: newValue);
      bind.mainUpdateTemporaryPassword();
      Future.delayed(Duration(milliseconds: 200), () {
        close();
        _showSuccess();
      });
    }

    return CustomAlertDialog(
      title: Text(translate("Set one-time password length")),
      content: Row(
          mainAxisAlignment: MainAxisAlignment.spaceEvenly,
          children: lengths
              .map(
                (value) => Row(
                  children: [
                    Text(value),
                    Radio(
                        value: value, groupValue: length, onChanged: setLength),
                  ],
                ),
              )
              .toList()),
    );
  }, backDismiss: true, clickMaskDismiss: true);
}

void showServerSettings(OverlayDialogManager dialogManager,
    void Function(VoidCallback) setState) async {
  Map<String, dynamic> options = {};
  try {
    options = jsonDecode(await bind.mainGetOptions());
  } catch (e) {
    print("Invalid server config: $e");
  }
  showServerSettingsWithOptions(options, dialogManager, setState);
}

void showServerSettingsWithValue(
    ServerConfig serverConfig,
    OverlayDialogManager dialogManager,
    void Function(VoidCallback)? upSetState) async {
  Map<String, dynamic> options = {};
  try {
    options = jsonDecode(await bind.mainGetOptions());
  } catch (_) {}
  options['custom-rendezvous-server'] = serverConfig.idServer;
  options['relay-server'] = serverConfig.relayServer;
  options['api-server'] = serverConfig.apiServer;
  options['key'] = serverConfig.key;
  showServerSettingsWithOptions(options, dialogManager, upSetState);
}

void showServerSettingsWithOptions(
    Map<String, dynamic> options,
    OverlayDialogManager dialogManager,
    void Function(VoidCallback)? upSetState) async {
  var isInProgress = false;

  List<ServerProfileItem> profiles = [];
  try {
    final rawProfiles = options['server-profiles'];
    if (rawProfiles != null && rawProfiles.toString().isNotEmpty) {
      final list = jsonDecode(rawProfiles.toString());
      if (list is List && list.isNotEmpty) {
        profiles = list
            .map((e) => ServerProfileItem.fromJson(Map<String, dynamic>.from(e)))
            .toList();
      }
    }
  } catch (_) {}

  if (profiles.isEmpty) {
    final custom = (options['custom-rendezvous-server'] ?? '').toString().trim();
    final relay = (options['relay-server'] ?? '').toString().trim();
    final api = (options['api-server'] ?? '').toString().trim();
    final key = (options['key'] ?? '').toString().trim();

    if (custom.isNotEmpty) {
      final parts = custom
          .split(RegExp(r'[;,\n]'))
          .map((s) => s.trim())
          .where((s) => s.isNotEmpty)
          .toList();
      if (parts.length > 1) {
        for (var i = 0; i < parts.length; i++) {
          profiles.add(ServerProfileItem(
            id: 'profile-${i + 1}',
            name: 'Server ${i + 1}',
            host: parts[i],
            relay: relay,
            api: api,
            key: key,
            enabled: true,
          ));
        }
      } else if (parts.isNotEmpty) {
        profiles.add(ServerProfileItem(
          id: 'profile-1',
          name: 'Server 1',
          host: parts.first,
          relay: relay,
          api: api,
          key: key,
          enabled: true,
        ));
      }
    }
  }

  if (profiles.isEmpty) {
    profiles.add(ServerProfileItem(
      id: 'profile-1',
      name: 'Server 1',
      host: '',
      relay: '',
      api: '',
      key: '',
      enabled: true,
    ));
  }

  int selectedIndex = 0;

  final nameCtrl = TextEditingController(text: profiles[0].name);
  final idCtrl = TextEditingController(text: profiles[0].host);
  final relayCtrl = TextEditingController(text: profiles[0].relay);
  final apiCtrl = TextEditingController(text: profiles[0].api);
  final keyCtrl = TextEditingController(text: profiles[0].key);

  void syncCurrentProfileFromControllers() {
    if (selectedIndex >= 0 && selectedIndex < profiles.length) {
      profiles[selectedIndex].name = nameCtrl.text.trim();
      profiles[selectedIndex].host = idCtrl.text.trim();
      profiles[selectedIndex].relay = relayCtrl.text.trim();
      profiles[selectedIndex].api = apiCtrl.text.trim();
      profiles[selectedIndex].key = keyCtrl.text.trim();
    }
  }

  void loadControllersFromProfile(int idx) {
    if (idx >= 0 && idx < profiles.length) {
      nameCtrl.text = profiles[idx].name;
      idCtrl.text = profiles[idx].host;
      relayCtrl.text = profiles[idx].relay;
      apiCtrl.text = profiles[idx].api;
      keyCtrl.text = profiles[idx].key;
    }
  }

  RxString idServerMsg = ''.obs;
  RxString relayServerMsg = ''.obs;
  RxString apiServerMsg = ''.obs;

  final controllers = [idCtrl, relayCtrl, apiCtrl, keyCtrl];
  final errMsgs = [
    idServerMsg,
    relayServerMsg,
    apiServerMsg,
  ];

  dialogManager.show((setState, close, context) {
    Future<bool> submit() async {
      syncCurrentProfileFromControllers();
      setState(() {
        isInProgress = true;
      });

      // Save full server profiles list
      final profilesJson = jsonEncode(profiles.map((p) => p.toJson()).toList());
      await bind.mainSetOption(key: 'server-profiles', value: profilesJson);

      // Save primary active profile to traditional options for backward compatibility
      final primary = profiles.firstWhereOrNull((p) => p.enabled && p.host.isNotEmpty) ??
          profiles.firstWhereOrNull((p) => p.host.isNotEmpty) ??
          profiles[0];

      bool ret = await setServerConfig(
          null,
          errMsgs,
          ServerConfig(
              idServer: primary.host,
              relayServer: primary.relay,
              apiServer: primary.api,
              key: primary.key));

      setState(() {
        isInProgress = false;
      });
      return ret;
    }

    Widget buildField(
        String label, TextEditingController controller, String errorMsg,
        {String? Function(String?)? validator,
        bool autofocus = false,
        void Function(String)? onChanged}) {
      if (isDesktop || isWeb) {
        return Row(
          children: [
            SizedBox(
              width: 120,
              child: Text(label),
            ),
            SizedBox(width: 8),
            Expanded(
              child: serverSettingsTextFormField(
                label: label,
                controller: controller,
                errorMsg: errorMsg,
                contentPadding:
                    EdgeInsets.symmetric(horizontal: 8, vertical: 12),
                showLabelText: false,
                validator: validator,
                autofocus: autofocus,
              ).workaroundFreezeLinuxMint(),
            ),
          ],
        );
      }

      return serverSettingsTextFormField(
        label: label,
        controller: controller,
        errorMsg: errorMsg,
        validator: validator,
      ).workaroundFreezeLinuxMint();
    }

    final curProfile = (selectedIndex >= 0 && selectedIndex < profiles.length)
        ? profiles[selectedIndex]
        : profiles[0];

    return CustomAlertDialog(
      title: Row(
        children: [
          Expanded(child: Text(translate('ID/Relay Server'))),
          Tooltip(
            message: translate('Import server config'),
            child: IconButton(
              icon: Icon(Icons.paste, color: Colors.grey),
              onPressed: () {
                Clipboard.getData(Clipboard.kTextPlain).then((value) {
                  final text = value?.text?.trim();
                  if (text != null && text.isNotEmpty) {
                    final imported = ServerProfileItem.decodeProfiles(text);
                    if (imported.isNotEmpty) {
                      setState(() {
                        profiles = imported;
                        selectedIndex = 0;
                        loadControllersFromProfile(0);
                      });
                      showToast(translate('Import server configuration successfully'));
                    } else {
                      showToast(translate('Invalid server configuration'));
                    }
                  } else {
                    showToast(translate('Clipboard is empty'));
                  }
                });
              },
            ),
          ),
          Tooltip(
            message: translate('Export Server Config'),
            child: IconButton(
              icon: Icon(Icons.copy, color: Colors.grey),
              onPressed: () {
                syncCurrentProfileFromControllers();
                final text = ServerProfileItem.encodeProfiles(profiles);
                Clipboard.setData(ClipboardData(text: text));
                showToast(translate('Export server configuration successfully'));
              },
            ),
          ),
        ],
      ),
      content: ConstrainedBox(
        constraints: const BoxConstraints(minWidth: 520),
        child: Form(
          child: Obx(() => Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  // Server Profile Tabs / Selector
                  SingleChildScrollView(
                    scrollDirection: Axis.horizontal,
                    child: Row(
                      children: [
                        for (int i = 0; i < profiles.length; i++) ...[
                          InkWell(
                            onTap: () {
                              setState(() {
                                syncCurrentProfileFromControllers();
                                selectedIndex = i;
                                loadControllersFromProfile(i);
                              });
                            },
                            borderRadius: BorderRadius.circular(8),
                            child: Container(
                              padding: EdgeInsets.symmetric(
                                  horizontal: 10, vertical: 6),
                              margin: EdgeInsets.only(right: 6, bottom: 8),
                              decoration: BoxDecoration(
                                color: selectedIndex == i
                                    ? Theme.of(context).colorScheme.primary.withOpacity(0.15)
                                    : Theme.of(context).cardColor,
                                border: Border.all(
                                  color: selectedIndex == i
                                      ? Theme.of(context).colorScheme.primary
                                      : Colors.grey.withOpacity(0.3),
                                ),
                                borderRadius: BorderRadius.circular(8),
                              ),
                              child: Row(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  Checkbox(
                                    value: profiles[i].enabled,
                                    materialTapTargetSize:
                                        MaterialTapTargetSize.shrinkWrap,
                                    visualDensity: VisualDensity.compact,
                                    onChanged: (val) {
                                      setState(() {
                                        profiles[i].enabled = val ?? true;
                                      });
                                    },
                                  ),
                                  Builder(builder: (context) {
                                    final stat = stateGlobal.serverStatuses.firstWhereOrNull(
                                        (s) => s.id == profiles[i].id || (profiles[i].host.isNotEmpty && s.host == profiles[i].host));
                                    final isOnline = stat?.online ?? false;
                                    final latStr = (stat != null && stat.latencyMs > 0) ? '${stat.latencyMs}ms' : '';
                                    return Row(
                                      mainAxisSize: MainAxisSize.min,
                                      children: [
                                        Text(
                                          profiles[i].name.isNotEmpty
                                              ? profiles[i].name
                                              : 'Server ${i + 1}',
                                          style: TextStyle(
                                            fontWeight: selectedIndex == i
                                                ? FontWeight.bold
                                                : FontWeight.normal,
                                            fontSize: 13,
                                          ),
                                        ),
                                        if (profiles[i].enabled && stat != null) ...[
                                          SizedBox(width: 5),
                                          Container(
                                            width: 7,
                                            height: 7,
                                            decoration: BoxDecoration(
                                              shape: BoxShape.circle,
                                              color: isOnline
                                                  ? Color.fromARGB(255, 50, 190, 166)
                                                  : Color.fromARGB(255, 224, 79, 95),
                                            ),
                                          ),
                                          if (latStr.isNotEmpty) ...[
                                            SizedBox(width: 3),
                                            Text(
                                              latStr,
                                              style: TextStyle(
                                                fontSize: 10,
                                                color: isOnline
                                                    ? Color.fromARGB(255, 50, 190, 166)
                                                    : Colors.grey,
                                              ),
                                            ),
                                          ],
                                        ],
                                      ],
                                    );
                                  }),
                                  if (profiles.length > 1) ...[
                                    SizedBox(width: 4),
                                    InkWell(
                                      onTap: () {
                                        setState(() {
                                          profiles.removeAt(i);
                                          if (selectedIndex >= profiles.length) {
                                            selectedIndex = profiles.length - 1;
                                          }
                                          loadControllersFromProfile(selectedIndex);
                                        });
                                      },
                                      child: Icon(Icons.close, size: 14, color: Colors.grey),
                                    ),
                                  ],
                                ],
                              ),
                            ),
                          ),
                        ],
                        IconButton(
                          icon: Icon(Icons.add_circle_outline, size: 20),
                          tooltip: translate('Add'),
                          onPressed: () {
                            setState(() {
                              syncCurrentProfileFromControllers();
                              final newIdx = profiles.length + 1;
                              profiles.add(ServerProfileItem(
                                id: 'profile-${DateTime.now().millisecondsSinceEpoch}',
                                name: 'Server $newIdx',
                                host: '',
                                relay: '',
                                api: '',
                                key: '',
                                enabled: true,
                              ));
                              selectedIndex = profiles.length - 1;
                              loadControllersFromProfile(selectedIndex);
                            });
                          },
                        ),
                      ],
                    ),
                  ),
                  Divider(height: 16),
                  buildField(
                    translate('Profile Name'),
                    nameCtrl,
                    '',
                    onChanged: (v) {
                      profiles[selectedIndex].name = v;
                      setState(() {});
                    },
                  ),
                  SizedBox(height: 8),
                  buildField(
                    translate('ID Server'),
                    idCtrl,
                    idServerMsg.value,
                    autofocus: true,
                    onChanged: (v) {
                      profiles[selectedIndex].host = v;
                    },
                  ),
                  SizedBox(height: 8),
                  if (!isIOS && !isWeb) ...[
                    buildField(
                      translate('Relay Server'),
                      relayCtrl,
                      relayServerMsg.value,
                      onChanged: (v) {
                        profiles[selectedIndex].relay = v;
                      },
                    ),
                    SizedBox(height: 8),
                  ],
                  buildField(
                    translate('API Server'),
                    apiCtrl,
                    apiServerMsg.value,
                    onChanged: (v) {
                      profiles[selectedIndex].api = v;
                    },
                    validator: (v) {
                      if (v != null && v.isNotEmpty) {
                        if (!(v.startsWith('http://') ||
                            v.startsWith("https://"))) {
                          return translate("invalid_http");
                        }
                      }
                      return null;
                    },
                  ),
                  SizedBox(height: 8),
                  buildField('Key', keyCtrl, '', onChanged: (v) {
                    profiles[selectedIndex].key = v;
                  }),
                  if (isInProgress)
                    Padding(
                      padding: EdgeInsets.only(top: 8),
                      child: LinearProgressIndicator(),
                    ),
                ],
              )),
        ),
      ),
      actions: [
        dialogButton('Cancel', onPressed: () {
          close();
        }, isOutline: true),
        dialogButton(
          'OK',
          onPressed: () async {
            if (await submit()) {
              close();
              showToast(translate('Successful'));
              upSetState?.call(() {});
            } else {
              showToast(translate('Failed'));
            }
          },
        ),
      ],
    );
  });
}

TextFormField serverSettingsTextFormField({
  required String label,
  required TextEditingController controller,
  required String errorMsg,
  String? Function(String?)? validator,
  bool autofocus = false,
  bool showLabelText = true,
  EdgeInsetsGeometry? contentPadding,
}) {
  return TextFormField(
    controller: controller,
    decoration: InputDecoration(
      labelText: showLabelText ? label : null,
      errorText: errorMsg.isEmpty ? null : errorMsg,
      contentPadding: contentPadding,
    ),
    validator: validator,
    autofocus: autofocus,
    keyboardType: TextInputType.visiblePassword,
    textCapitalization: TextCapitalization.none,
    autocorrect: false,
    enableSuggestions: false,
    smartDashesType: SmartDashesType.disabled,
    smartQuotesType: SmartQuotesType.disabled,
    enableIMEPersonalizedLearning: false,
    spellCheckConfiguration: const SpellCheckConfiguration.disabled(),
  );
}

void setPrivacyModeDialog(
  OverlayDialogManager dialogManager,
  List<TToggleMenu> privacyModeList,
  RxString privacyModeState,
) async {
  dialogManager.dismissAll();
  dialogManager.show((setState, close, context) {
    return CustomAlertDialog(
      title: Text(translate('Privacy mode')),
      content: Column(
          mainAxisAlignment: MainAxisAlignment.spaceEvenly,
          children: privacyModeList
              .map((value) => CheckboxListTile(
                    contentPadding: EdgeInsets.zero,
                    visualDensity: VisualDensity.compact,
                    title: value.child,
                    value: value.value,
                    onChanged: value.onChanged,
                  ))
              .toList()),
    );
  }, backDismiss: true, clickMaskDismiss: true);
}
