use std::ops::Sub;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum PaymentsApiRequest {
    #[serde(rename = "list-payment-methods")]
    ListPaymentMethods,

    #[serde(rename = "create-payment-method-add-link")]
    CreatePaymentMethodAddLink,

    #[serde(rename = "delete-payment-method")]
    DeletePaymentMethod(DeletePaymentMethod),

    #[serde(rename = "create-cart-subscription")]
    CreateCartSubscription(CreateCartSubscription),

    #[serde(rename = "checkout-subscriptions")]
    CheckoutSubscriptions,

    #[serde(rename = "list-subscriptions")]
    ListSubscriptions(ListSubscriptions),
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct CreateCartSubscription {
    pub subscription_type: SubscriptionType,
    pub subscription_interval: SubscriptionInterval,
    pub auto_renew: bool,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct DeletePaymentMethod {
    pub payment_method_id: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, JsonSchema)]
pub enum SubscriptionType {
    #[serde(rename = "custom-domain")]
    CustomDomain,
    #[serde(rename = "dedicated-ip")]
    DedicatedIP,
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, JsonSchema)]
pub enum SubscriptionInterval {
    #[serde(rename = "month")]
    Month,
    #[serde(rename = "quarter")]
    Quarter,
    #[serde(rename = "year")]
    Year,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct ListSubscriptions {
    active_only: bool,
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
#[serde(tag = "type")]
pub enum PaymentsApiResponse {
    #[serde(rename = "payment-methods")]
    PaymentMethods {
        payment_methods: Vec<PaymentMethod>,
    },
    #[serde(rename = "add-payment-link")]
    AddPaymentMethodLink {
        url: String,
    },
    #[serde(rename = "payment-method-removed")]
    PaymentMethodRemoved,
    #[serde(rename = "cart-subscription-created")]
    CartSubscriptionCreated {
        sub_id: Uuid,
    },
    #[serde(rename = "invoice-pending")]
    InvoicePending {
        invoice_id: Uuid,
    },
    #[serde(rename = "subscriptions")]
    Subscriptions {
        subscriptions: Vec<Subscription>,
    },
}

#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct PaymentMethod {
    pub id: String,
    pub display_text: String,
    pub brand: String,
}


#[derive(Serialize, Deserialize, Debug, JsonSchema)]
pub struct Subscription {
    pub id: String,
}

