import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitDiscordCreateProperties, BenefitDiscordCreateProperties$Outbound } from "./benefitdiscordcreateproperties.js";
export type BenefitDiscordUpdate = {
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description?: string | null | undefined;
    type?: "discord" | undefined;
    properties?: BenefitDiscordCreateProperties | null | undefined;
};
/** @internal */
export declare const BenefitDiscordUpdate$inboundSchema: z.ZodType<BenefitDiscordUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitDiscordUpdate$Outbound = {
    description?: string | null | undefined;
    type: "discord";
    properties?: BenefitDiscordCreateProperties$Outbound | null | undefined;
};
/** @internal */
export declare const BenefitDiscordUpdate$outboundSchema: z.ZodType<BenefitDiscordUpdate$Outbound, z.ZodTypeDef, BenefitDiscordUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitDiscordUpdate$ {
    /** @deprecated use `BenefitDiscordUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitDiscordUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitDiscordUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitDiscordUpdate$Outbound, z.ZodTypeDef, BenefitDiscordUpdate>;
    /** @deprecated use `BenefitDiscordUpdate$Outbound` instead. */
    type Outbound = BenefitDiscordUpdate$Outbound;
}
export declare function benefitDiscordUpdateToJSON(benefitDiscordUpdate: BenefitDiscordUpdate): string;
export declare function benefitDiscordUpdateFromJSON(jsonString: string): SafeParseResult<BenefitDiscordUpdate, SDKValidationError>;
//# sourceMappingURL=benefitdiscordupdate.d.ts.map