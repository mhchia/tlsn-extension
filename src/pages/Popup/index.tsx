import React from 'react';
import { createRoot } from 'react-dom/client';
import { HashRouter } from 'react-router-dom';

import Popup from './Popup';
import './index.scss';
import { Provider } from 'react-redux';
import store from '../../utils/store';
import { BackgroundActiontype } from '../Background/actionTypes';

const container = document.getElementById('app-container');
const root = createRoot(container!); // createRoot(container!) if you use TypeScript

chrome.runtime.onMessage.addListener((request) => {
  switch (request.type) {
    case BackgroundActiontype.push_action: {
      if (request.data.tabId === store.getState().requests.activeTab?.id) {
        store.dispatch(request.action);
      }
      break;
    }
  }
});

root.render(
  <Provider store={store}>
    <HashRouter>
      <Popup />
    </HashRouter>
  </Provider>,
);
