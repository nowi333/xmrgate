( function () {
	var registry = window.wc && window.wc.wcBlocksRegistry;
	var settings = window.wc && window.wc.wcSettings;
	if ( ! registry || ! settings ) {
		return;
	}
	var data = settings.getSetting( 'xmrgate_data', {} );
	var decode = window.wp.htmlEntities.decodeEntities;
	var el = window.wp.element.createElement;
	var title = decode( data.title || 'Monero (XMR)' );

	function Description() {
		return el( 'div', null, decode( data.description || '' ) );
	}

	registry.registerPaymentMethod( {
		name: 'xmrgate',
		label: el( 'span', null, title ),
		ariaLabel: title,
		content: el( Description ),
		edit: el( Description ),
		canMakePayment: function () {
			return true;
		},
		supports: { features: data.supports || [ 'products' ] },
	} );
} )();
