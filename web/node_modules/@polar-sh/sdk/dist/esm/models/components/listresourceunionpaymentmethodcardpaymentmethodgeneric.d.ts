import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
import { PaymentMethodCard, PaymentMethodCard$Outbound } from "./paymentmethodcard.js";
import { PaymentMethodGeneric, PaymentMethodGeneric$Outbound } from "./paymentmethodgeneric.js";
export type Items = PaymentMethodGeneric | PaymentMethodCard;
export type ListResourceUnionPaymentMethodCardPaymentMethodGeneric = {
    items: Array<PaymentMethodGeneric | PaymentMethodCard>;
    pagination: Pagination;
};
/** @internal */
export declare const Items$inboundSchema: z.ZodType<Items, z.ZodTypeDef, unknown>;
/** @internal */
export type Items$Outbound = PaymentMethodGeneric$Outbound | PaymentMethodCard$Outbound;
/** @internal */
export declare const Items$outboundSchema: z.ZodType<Items$Outbound, z.ZodTypeDef, Items>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Items$ {
    /** @deprecated use `Items$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Items, z.ZodTypeDef, unknown>;
    /** @deprecated use `Items$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Items$Outbound, z.ZodTypeDef, Items>;
    /** @deprecated use `Items$Outbound` instead. */
    type Outbound = Items$Outbound;
}
export declare function itemsToJSON(items: Items): string;
export declare function itemsFromJSON(jsonString: string): SafeParseResult<Items, SDKValidationError>;
/** @internal */
export declare const ListResourceUnionPaymentMethodCardPaymentMethodGeneric$inboundSchema: z.ZodType<ListResourceUnionPaymentMethodCardPaymentMethodGeneric, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound = {
    items: Array<PaymentMethodGeneric$Outbound | PaymentMethodCard$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceUnionPaymentMethodCardPaymentMethodGeneric$outboundSchema: z.ZodType<ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound, z.ZodTypeDef, ListResourceUnionPaymentMethodCardPaymentMethodGeneric>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceUnionPaymentMethodCardPaymentMethodGeneric$ {
    /** @deprecated use `ListResourceUnionPaymentMethodCardPaymentMethodGeneric$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceUnionPaymentMethodCardPaymentMethodGeneric, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceUnionPaymentMethodCardPaymentMethodGeneric$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound, z.ZodTypeDef, ListResourceUnionPaymentMethodCardPaymentMethodGeneric>;
    /** @deprecated use `ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound` instead. */
    type Outbound = ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound;
}
export declare function listResourceUnionPaymentMethodCardPaymentMethodGenericToJSON(listResourceUnionPaymentMethodCardPaymentMethodGeneric: ListResourceUnionPaymentMethodCardPaymentMethodGeneric): string;
export declare function listResourceUnionPaymentMethodCardPaymentMethodGenericFromJSON(jsonString: string): SafeParseResult<ListResourceUnionPaymentMethodCardPaymentMethodGeneric, SDKValidationError>;
//# sourceMappingURL=listresourceunionpaymentmethodcardpaymentmethodgeneric.d.ts.map